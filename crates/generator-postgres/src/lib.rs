use std::{
	collections::{HashMap, HashSet},
	fmt::{self, Display},
	ops::{Deref, DerefMut},
};

use itertools::Itertools;
use schema::{
	ChangeList, ColumnDiff, Diff, EnumDiff, HasDefaultDbName, HasIdent, HasUid, IsCompatible,
	IsIsomorph, SchemaComposite, SchemaDiff, SchemaEnum, SchemaItem, SchemaScalar, SchemaSql,
	SchemaTable, SchemaTableOrView, SchemaType, SchemaView, TableCheck, TableColumn, TableDiff,
	TableForeignKey, TableIndex, TablePrimaryKey, TableSql, TableUniqueConstraint,
	diagnostics::Report,
	ids::{DbIdent, Ident},
	index::Check,
	mk_change_list,
	names::{
		ConstraintKind, DbColumn, DbConstraint, DbEnumItem, DbIndex, DbItem, DbTable, DbType,
		DbView,
	},
	renamelist::RenameOp,
	root::Schema,
	scalar::{EnumItemHandle, ScalarAnnotation},
	sql::{Sql, SqlOp},
	uid::{RenameExt, RenameMap},
	view::DefinitionPart,
	w, wl,
};

use crate::escape::escape_identifier;

mod escape;
pub mod validate;

#[derive(Clone, Copy, Debug)]
pub struct Pg<T>(pub T);
impl<T> Deref for Pg<T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}
impl<T> DerefMut for Pg<T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.0
	}
}
impl<T> HasUid for Pg<T>
where
	T: HasUid,
{
	fn uid(&self) -> schema::uid::Uid {
		self.0.uid()
	}
}
impl<T> HasDefaultDbName for Pg<T>
where
	T: HasDefaultDbName,
{
	type Kind = T::Kind;

	fn default_db(&self) -> Option<DbIdent<Self::Kind>> {
		self.0.default_db()
	}
}

#[derive(Debug)]
pub struct Alternation {
	groupable_up: bool,
	groupable_down: bool,
	alt: String,
}
macro_rules! alt_group {
    ($($tt:tt)*) => {
        Alternation {
				groupable_up: true,
				groupable_down: true,
				alt: format!($($tt)*),
		  }
    };
}
macro_rules! alt_ungroup {
    ($($tt:tt)*) => {
        Alternation {
				groupable_up: false,
				groupable_down: false,
				alt: format!($($tt)*),
		  }
    };
}
macro_rules! alt_ungroup_up {
    ($($tt:tt)*) => {
        Alternation {
				groupable_up: false,
				groupable_down: true,
				alt: format!($($tt)*),
		  }
    };
}

impl Pg<TableColumn<'_>> {
	pub fn create_inline(&self, sql: &mut String, rn: &RenameMap, report: &mut Report) {
		self.create_inline_inner(sql, rn, false, report);
	}
	fn create_inline_inner(
		&self,
		sql: &mut String,
		rn: &RenameMap,
		force_nullable: bool,
		report: &mut Report,
	) {
		let name = Id(self.db(rn));
		let db_type = self.db_type(rn, report);
		let nullability = if self.nullable || force_nullable {
			""
		} else {
			" NOT NULL"
		};
		let defaultability = self.default().map_or_else(String::new, |default| {
			let sql = Pg(self.table).format_sql(default, rn, report);
			format!(" DEFAULT ({sql})")
		});
		w!(sql, "{name} {}{nullability}{defaultability}", db_type.raw());
	}
	pub fn drop_alter(&self, out: &mut Vec<Alternation>, rn: &RenameMap) {
		let name = Id(self.db(rn));
		out.push(alt_group!("DROP COLUMN {name}"));
	}
	pub fn create_alter(&self, out: &mut Vec<Alternation>, rn: &RenameMap, report: &mut Report) {
		let mut inl = String::new();
		self.create_inline_inner(&mut inl, rn, self.initialize_as().is_some(), report);
		out.push(alt_group!("ADD COLUMN {inl}"));
		if let Some(initialize_as) = self.initialize_as() {
			let name = Id(self.db(rn));
			let db_type = self.db_type(rn, report);
			let sql = format_sql(
				initialize_as,
				self.table.schema,
				SchemaItem::Table(self.table),
				rn,
				report,
			);
			// Technically groupable, yet postgres bails with error: column "column" of relation "tests" does not exist,
			// if appears with the same statement as ADD COLUMN.
			out.push(alt_ungroup_up!(
				"ALTER COLUMN {name} SET DATA TYPE {} USING {sql}",
				db_type.raw(),
			));
			if !self.nullable {
				out.push(alt_group!("ALTER COLUMN {name} SET NOT NULL"));
			}
		}
	}
	/// Column renaming doesn't support grouping multiple renames in one ALTER TABLE.
	pub fn rename_alter(&self, to: DbColumn, out: &mut Vec<Alternation>, rn: &mut RenameMap) {
		let name = Id(self.db(rn));
		out.push(alt_ungroup!("RENAME COLUMN {name} TO {}", Id(&to)));
		self.set_db(rn, to);
	}
}
impl Pg<TableSql<'_>> {
	pub fn print(&self, sql: &mut String, rn: &RenameMap, report: &mut Report) {
		let o = format_sql(
			&self.0,
			self.table.schema,
			SchemaItem::Table(self.0.table),
			rn,
			report,
		);
		sql.push_str(&o);
	}
}

fn pg_constraints<'v>(t: &'v SchemaTable) -> Vec<PgTableConstraint<'v>> {
	let mut out = vec![];
	if let Some(pk) = t.primary_key() {
		out.push(PgTableConstraint::PrimaryKey(pk));
	}
	for check in t.checks() {
		out.push(PgTableConstraint::Check(check));
	}
	for unique in t.unique_constraints() {
		out.push(PgTableConstraint::Unique(unique));
	}
	for fk in t.foreign_keys() {
		out.push(PgTableConstraint::ForeignKey(fk));
	}
	out
}

impl Pg<SchemaTable<'_>> {
	fn format_sql(&self, sql: &Sql, rn: &RenameMap, report: &mut Report) -> String {
		let mut out = String::new();
		Pg(self.sql(sql)).print(&mut out, rn, report);
		out
	}
	pub fn create(&self, sql: &mut String, rn: &RenameMap, report: &mut Report) {
		let table_name = Id(self.db(rn));
		wl!(sql, "CREATE TABLE {table_name} (");
		let mut had = false;
		for v in self.columns() {
			if had {
				w!(sql, ",");
			} else {
				had = true;
			};
			w!(sql, "\t");
			Pg(v).create_inline(sql, rn, report);
			// assert!(
			// 	v.initialize_as().is_none(),
			// 	"@initialize_as may only appear when field is added, not when the table is created"
			// );
			wl!(sql, "");
		}
		let constraints = pg_constraints(self);
		for constraint in constraints {
			let Some(inline) = constraint.create_inline_non_fk(rn, report) else {
				continue;
			};
			if had {
				w!(sql, ",");
			} else {
				had = true;
			};
			wl!(sql, "\t{inline}");
		}
		wl!(sql, ");");
		for idx in self.indexes() {
			Pg(idx).create(sql, rn);
		}

		// Add comments
		for column in self.columns() {
			let column_name = Id(column.db(rn));
			let new_c = docs_to_string(column.docs.clone());

			if !new_c.is_empty() {
				let doc = new_c.replace('\'', "''");
				wl!(
					sql,
					"COMMENT ON COLUMN {table_name}.{column_name} IS '{doc}';"
				);
			}
		}
	}
	pub fn comment_id(&self, rn: &RenameMap) -> String {
		let table_name = Id(self.db(rn));
		format!("TABLE {table_name}")
	}
	pub fn drop(&self, sql: &mut String, rn: &RenameMap) {
		let table_name = Id(self.db(rn));
		wl!(sql, "DROP TABLE {table_name};");
	}
	pub fn rename(&self, to: DbTable, sql: &mut String, rn: &mut RenameMap, external: bool) {
		if !external {
			self.print_alternations(&[alt_ungroup!("RENAME TO {}", Id(&to))], sql, rn);
		}
		self.set_db(rn, to);
	}

	pub fn print_alternations(&self, mut out: &[Alternation], sql: &mut String, rn: &RenameMap) {
		fn print_group(name: &DbTable, list: &[Alternation], sql: &mut String) {
			let name = Id(name);
			if list.len() > 1 {
				w!(sql, "ALTER TABLE {name}\n");
				for (i, alt) in list.iter().enumerate() {
					if i != 0 {
						w!(sql, ",");
					};
					wl!(sql, "\t{}", alt.alt);
				}
				wl!(sql, ";");
			} else {
				let alt = &list[0];
				w!(sql, "ALTER TABLE {name} {};\n", alt.alt);
			}
		}
		let name = &self.db(rn);
		while !out.is_empty() {
			let mut count = 1;
			loop {
				if !out[count - 1].groupable_down || out.len() == count {
					break;
				}
				if !out[count].groupable_up {
					break;
				}
				count += 1;
			}
			print_group(name, &out[..count], sql);
			out = &out[count..];
		}
	}
}

impl Pg<ColumnDiff<'_>> {
	pub fn print_alter(
		&self,
		out: &mut Vec<Alternation>,
		rn: &RenameMap,
		report_old: &mut Report,
		report_new: &mut Report,
	) {
		let name = Id(self.new.db(rn));
		let new_ty = self.new.db_type(rn, report_new);
		if self.old.db_type(rn, report_old) != new_ty {
			if let Some(initialize_as) = self.new.initialize_as() {
				let sql = format_sql(
					initialize_as,
					self.new.table.schema,
					SchemaItem::Table(self.new.table),
					rn,
					report_new,
				);
				out.push(alt_group!(
					"ALTER COLUMN {name} SET DATA TYPE {} USING {sql}",
					new_ty.raw()
				));
			} else {
				out.push(alt_group!(
					"ALTER COLUMN {name} SET DATA TYPE {}",
					new_ty.raw()
				));
			}
		}
		let new_nullable = self.new.nullable;
		if self.old.nullable != new_nullable {
			if new_nullable {
				out.push(alt_group!("ALTER COLUMN {name} DROP NOT NULL"));
			} else {
				out.push(alt_group!("ALTER COLUMN {name} SET NOT NULL"));
			}
		}

		let old_default = self.old.default();
		let new_default = self.new.default();
		match (old_default, new_default) {
			(None, Some(new_default)) => out.push(alt_group!(
				"ALTER COLUMN {name} SET DEFAULT {}",
				Pg(self.new.table).format_sql(new_default, rn, report_new)
			)),
			(Some(_), None) => out.push(alt_group!("ALTER COLUMN {name} DROP DEFAULT")),
			(Some(old_default), Some(new_default)) => {
				let old_default = Pg(self.old.table).format_sql(old_default, rn, report_old);
				let new_default = Pg(self.new.table).format_sql(new_default, rn, report_new);
				if new_default != old_default {
					out.push(alt_group!("ALTER COLUMN {name} SET DEFAULT {new_default}",));
				}
			}
			(None, None) => {}
		}

		// #[allow(clippy::match_same_arms)]
		// match (self.old.initialize_as(), self.new.initialize_as()) {
		// 	(Some(_), Some(_)) => panic!("@initialize_as may not be left on migration"),
		// 	(None, Some(_)) => {
		// 		// TODO: Disallow when generating up migration.
		// 		//panic!("@initialize_as may only be dropped")
		// 	}
		// 	(Some(_) | None, None) => {}
		// }
	}
}
impl Pg<TableDiff<'_>> {
	pub fn print_stage1(
		&self,
		sql: &mut String,
		rn: &mut RenameMap,
		external: bool,
		report_old: &mut Report,
		report_new: &mut Report,
	) -> ChangeList<TableColumn<'_>> {
		let mut out = Vec::new();

		let old_columns = self.old.columns().collect::<Vec<_>>();
		let new_columns = self.new.columns().collect::<Vec<_>>();
		let column_changes = mk_change_list(
			rn,
			&old_columns,
			&new_columns,
			|v| v,
			report_old,
			report_new,
		);
		// Rename/moveaway columns
		{
			let mut updated = HashMap::new();
			for ele in &column_changes.renamed {
				match ele {
					RenameOp::Rename(i, r, _) => {
						Pg(*i).rename_alter(DbIdent::unchecked_from(r.clone()), &mut out, rn);
					}
					RenameOp::Store(i, t) => {
						Pg(*i).rename_alter(t.db(), &mut out, rn);
						updated.insert(t, i);
					}
					RenameOp::Moveaway(i, t) => {
						Pg(*i).rename_alter(t.db(), &mut out, rn);
					}
					RenameOp::Restore(t, n, _) => {
						let table = updated.remove(&t).expect("stored");
						Pg(*table).rename_alter(DbIdent::unchecked_from(n.clone()), &mut out, rn);
					}
				};
			}
		}

		if !external {
			Pg(self.new).print_alternations(&out, sql, rn);
		}
		column_changes
	}

	pub fn print_stage1_5(
		&self,
		sql: &mut String,
		rn: &RenameMap,
		external: bool,
		report_old: &mut Report,
		report_new: &mut Report,
	) -> ChangeList<PgTableConstraint<'_>> {
		let mut out = vec![];
		// Drop fks
		let old_constraints = pg_constraints(&self.old);
		let new_constraints = pg_constraints(&self.new);
		let constraint_changes = mk_change_list(
			rn,
			&old_constraints,
			&new_constraints,
			|v| v,
			report_old,
			report_new,
		);
		for constraint in &constraint_changes.dropped {
			constraint.drop_alter_fk(&mut out, rn);
		}
		if !external {
			Pg(self.new).print_alternations(&out, sql, rn);
		}
		constraint_changes
	}

	pub fn print_stage2(
		&self,
		sql: &mut String,
		rn: &mut RenameMap,
		column_changes: &ChangeList<TableColumn<'_>>,
		constraint_changes: ChangeList<PgTableConstraint<'_>>,
		external: bool,
		report_old: &mut Report,
		report_new: &mut Report,
	) -> Vec<Alternation> {
		let mut out = Vec::new();

		// Drop constraints
		for constraint in constraint_changes.dropped {
			constraint.drop_alter_non_fk(&mut out, rn);
		}
		// Rename constraints
		for ele in constraint_changes.renamed {
			let mut stored = HashMap::new();
			match ele {
				RenameOp::Rename(a, b, _) => {
					a.rename_alter(b, &mut out, rn);
				}
				RenameOp::Store(a, b) => {
					a.rename_alter(b.db(), &mut out, rn);
					stored.insert(b, a);
				}
				RenameOp::Restore(r, t, _) => {
					stored
						.remove(&r)
						.expect("stored")
						.rename_alter(t, &mut out, rn);
				}
				RenameOp::Moveaway(_, _) => {
					// All moveaways were dropped
				}
			}
		}

		// Drop old indexes
		let old_indexes = self.old.indexes().map(Pg).collect_vec();
		let new_indexes = self.new.indexes().map(Pg).collect_vec();
		let index_changes = mk_change_list(
			rn,
			&old_indexes,
			&new_indexes,
			|v| v,
			report_old,
			report_new,
		);
		for index in index_changes.dropped {
			index.drop(sql, rn);
		}

		// Create new columns
		for ele in &column_changes.created {
			Pg(*ele).create_alter(&mut out, rn, report_new);
		}

		// Update columns
		for ele in &column_changes.updated {
			Pg(*ele).print_alter(&mut out, rn, report_old, report_new);
		}

		// Update/create constraints except for foreign keys
		let mut fks = vec![];
		for constraint in constraint_changes.created {
			constraint.create_alter_non_fk(&mut out, rn, report_new);
			constraint.create_alter_fk(&mut fks, rn);
		}

		if !external {
			Pg(self.new).print_alternations(&out, sql, rn);
			out = vec![];
		}

		// Create/update indexes
		for added in index_changes.created {
			added.create(sql, rn);
		}
		for updated in index_changes.updated {
			// There is nothing updateable in indexes except for names, `old` should be equal to `new`
			// Indexes should be always dropped and recreated
			let _old_name = updated.old.db(rn);
			let new_name = updated.new.db(rn);
			updated.old.rename(new_name, sql, rn);
		}

		if !external {
			Pg(self.new).print_alternations(&out, sql, rn);
		}

		fks
	}
	pub fn print_stage3(
		&self,
		sql: &mut String,
		rn: &RenameMap,
		column_changes: ChangeList<TableColumn<'_>>,
		external: bool,
	) {
		let mut out = Vec::new();

		// Drop old columns
		for column in column_changes.dropped {
			Pg(column).drop_alter(&mut out, rn);
		}

		if !external {
			Pg(self.new).print_alternations(&out, sql, rn);
		}

		// Reconcile comments on table items
		// Note that comments on table creation are not handled here, see `Pg::<SchemaTable>::create`
		for ele in column_changes.created {
			let table_name = Id(self.old.db(rn));
			let column_name = Id(ele.db(rn));
			let new_c = docs_to_string(ele.docs.clone());

			if !new_c.is_empty() {
				let doc = new_c.replace('\'', "''");
				wl!(
					sql,
					"COMMENT ON COLUMN {table_name}.{column_name} IS '{doc}';"
				);
			}
		}
		for ele in column_changes.updated {
			let table_name = Id(self.old.db(rn));
			let column_name = Id(ele.old.db(rn));
			let old_c = docs_to_string(ele.old.docs.clone());
			let new_c = docs_to_string(ele.new.docs.clone());

			if old_c == new_c {
				continue;
			}
			if new_c.is_empty() {
				wl!(sql, "COMMENT ON COLUMN {table_name}.{column_name} IS NULL;");
			} else {
				let doc = new_c.replace('\'', "''");
				wl!(
					sql,
					"COMMENT ON COLUMN {table_name}.{column_name} IS '{doc}';"
				);
			}
		}
	}
}

impl Pg<TableForeignKey<'_>> {
	pub fn create_alter(&self, out: &mut Vec<Alternation>, rn: &RenameMap) {
		let mut alt = String::new();

		let name = Id(self.db(rn));
		w!(alt, "ADD CONSTRAINT {name} FOREIGN KEY(");
		let source_columns = self.source_columns();
		self.table
			.print_column_list(&mut alt, source_columns.into_iter(), rn);
		w!(alt, ") REFERENCES ");
		let target_table = self.target_table();
		let target_columns = self.target_columns();
		let target_table_name = Id(target_table.db(rn));
		w!(alt, "{target_table_name}(");
		target_table.print_column_list(&mut alt, target_columns.into_iter(), rn);
		w!(alt, ")");
		if let Some(on_delete) = self.on_delete.sql() {
			w!(alt, " ON DELETE {on_delete}");
		}

		out.push(Alternation {
			groupable_up: true,
			groupable_down: true,
			alt,
		});
	}
}
impl IsIsomorph for Pg<TableIndex<'_>> {
	fn is_isomorph(
		&self,
		other: &Self,
		rn: &RenameMap,
		report_self: &mut Report,
		report_other: &mut Report,
	) -> bool {
		let old_fields = self.db_columns(rn).collect_vec();
		let new_fields = other.db_columns(rn).collect_vec();
		// TODO: Is it allowed to change column types, if there is an active index exists?
		// If not - then type equality should also be checked
		old_fields == new_fields
	}
}
impl IsCompatible for Pg<TableIndex<'_>> {
	fn is_compatible(
		&self,
		new: &Self,
		rn: &RenameMap,
		report_self: &mut Report,
		report_other: &mut Report,
	) -> bool {
		let old_column_opclass = self.db_columns_opclass(rn).collect_vec();
		let new_column_opclass = new.db_columns_opclass(rn).collect_vec();
		// We can't update unique flag, so they are not compatible and need to be recreated
		self.unique == new.unique
			&& self.using == new.using
			&& old_column_opclass == new_column_opclass
	}
}
impl Pg<TableIndex<'_>> {
	pub fn create(&self, sql: &mut String, rn: &RenameMap) {
		let name = Id(self.db(rn));
		let table_name = Id(self.table.db(rn));

		w!(sql, "CREATE ");
		if self.unique {
			w!(sql, "UNIQUE ");
		}
		w!(sql, "INDEX {name} ON {table_name}");
		if let Some(using) = &self.using {
			w!(sql, " USING {}", Id(using.0.as_str()));
		}
		w!(sql, "(\n");
		for (i, (c, opclass)) in self.db_columns_opclass(rn).enumerate() {
			if i != 0 {
				w!(sql, ",");
			}
			w!(sql, "\t{}", Id(c));
			if let Some(opclass) = opclass {
				w!(sql, " {}", Id(opclass.0.as_str()));
			}
			w!(sql, "\n");
		}
		w!(sql, ")");
		if let Some(with) = &self.with {
			w!(sql, " WITH ({})", with.0);
		}
		wl!(sql, ";");
	}
	pub fn drop(&self, sql: &mut String, rn: &RenameMap) {
		let name = Id(self.db(rn));
		w!(sql, "DROP INDEX {name};\n");
	}
	pub fn rename(&self, new_name: DbIndex, sql: &mut String, rn: &RenameMap) {
		let name = Id(self.db(rn));
		let new_name = Id(new_name);
		if name == new_name {
			return;
		}
		w!(sql, "ALTER INDEX {name} RENAME TO {new_name};\n");
		// TODO: missing set_db?
	}
}
impl Pg<SchemaDiff<'_>> {
	#[allow(clippy::too_many_lines)]
	pub(crate) fn print(
		&self,
		sql: &mut String,
		rn: &mut RenameMap,
		report_old: &mut Report,
		report_new: &mut Report,
	) {
		let changelist = self.changelist(rn, report_old, report_new);

		// Rename/moveaway everything
		for ele in changelist.renamed {
			let mut stored = HashMap::new();
			match ele {
				RenameOp::Store(i, t) => {
					stored.insert(t, i);
					Pg(i).rename(t.db(), sql, rn, i.is_external());
				}
				RenameOp::Moveaway(i, t) => {
					Pg(i).rename(t.db(), sql, rn, i.is_external());
				}
				RenameOp::Restore(t, n, r) => {
					let item = stored.remove(&t).expect("stored");
					Pg(item).rename(n, sql, rn, r.is_external());
				}
				RenameOp::Rename(v, n, r) => Pg(v).rename(n, sql, rn, r.is_external()),
			}
		}

		// Enums: print_renamed_added
		for ele in changelist
			.updated
			.iter()
			.filter(|d| matches!(d.old, SchemaItem::Enum(_)))
			.map(|ele| {
				let old = ele.old.as_enum().expect("enum");
				let new = ele.new.as_enum().expect("enum");
				Diff { old, new }
			}) {
			Pg(ele).print_renamed_added(sql, rn, report_old, report_new);
		}

		let mut initialized_tys = HashSet::new();
		for ele in &changelist.updated {
			// If item is updated - it must be already initialized
			initialized_tys.insert(Ident::unchecked_cast(ele.new.id()));
		}

		// Inlined/external scalars are always initialized
		for ele in self
			.new
			.items()
			.iter()
			.filter_map(SchemaItem::as_scalar)
			.filter(|s| s.inlined() || s.is_external())
		{
			initialized_tys.insert(ele.id());
		}

		// Create new types, in toposorted order
		{
			let mut remaining_types = changelist
				.created
				.iter()
				.filter_map(SchemaItem::as_type)
				.filter(|t| !initialized_tys.contains(&t.ident()))
				.collect_vec();
			if !remaining_types.is_empty() {
				loop {
					let (ready, pending) = remaining_types
						.iter()
						.partition::<Vec<SchemaType<'_>>, _>(|c| {
							let mut deps = Vec::new();
							c.type_dependencies(&mut deps);
							deps.iter().all(|f| initialized_tys.contains(f))
						});
					if ready.is_empty() {
						let mut e =
							report_new.error("circular dependency while creating new types");
						for ele in pending {
							e.annotate("cycle part", ele.ident().span());
						}
						break;
					}
					for ele in ready {
						Pg(ele).create(sql, rn, report_new);
						initialized_tys.insert(ele.ident());
					}
					if pending.is_empty() {
						break;
					}
					remaining_types = pending;
				}
			}
		}

		// Update scalars
		for ele in changelist
			.updated
			.iter()
			.filter(|d| matches!(d.old, SchemaItem::Scalar(_)))
			.map(|ele| {
				let old = ele.old.as_scalar().expect("scalar");
				let new = ele.new.as_scalar().expect("scalar");
				Pg(Diff { old, new })
			}) {
			let is_external = ele.new.is_external();
			Pg(ele).print(sql, rn, is_external, report_old, report_new);
		}

		// Create new tables
		for ele in changelist.created.iter().filter_map(SchemaItem::as_table) {
			if ele.is_external() {
				continue;
			}
			Pg(ele).create(sql, rn, report_new);
		}

		// Update tables
		let diffs = changelist
			.updated
			.iter()
			.filter(|d| matches!(d.old, SchemaItem::Table(_)))
			.map(|ele| {
				let old = ele.old.as_table().expect("table");
				let new = ele.new.as_table().expect("table");
				Pg(Diff { old, new })
			})
			.collect_vec();
		let mut changes = vec![];
		for diff in &diffs {
			let changelist =
				diff.print_stage1(sql, rn, diff.new.is_external(), report_old, report_new);
			changes.push(changelist);
		}
		let mut fksd = vec![];
		for diff in &diffs {
			let changelist =
				diff.print_stage1_5(sql, rn, diff.new.is_external(), report_old, report_new);
			fksd.push(changelist);
		}

		// Drop old views, in toposorted order
		{
			let mut remaining_views = changelist
				.dropped
				.iter()
				.filter_map(SchemaItem::as_view)
				.collect_vec();
			if !remaining_views.is_empty() {
				let mut sorted = vec![];
				let mut dependencies = remaining_views
					.iter()
					.map(|c| c.id())
					.collect::<HashSet<_>>();
				loop {
					let (ready, pending) = remaining_views
						.iter()
						.partition::<Vec<SchemaView<'_>>, _>(|c| {
							for def in &c.definition.0 {
								match def {
									DefinitionPart::Raw(_) => {}
									DefinitionPart::TableRef(t) => {
										if dependencies.contains(&Ident::unchecked_cast(*t)) {
											return false;
										}
									}
									DefinitionPart::ColumnRef(t, _) => {
										if dependencies.contains(&Ident::unchecked_cast(*t)) {
											return false;
										}
									}
								}
							}
							true
						});
					if ready.is_empty() {
						let mut e =
							report_new.error("circular dependency while dropping old views");
						for ele in pending {
							e.annotate("cycle part", ele.id().span());
						}
						break;
					}
					for ele in ready.into_iter().rev() {
						sorted.push(ele);
						dependencies.remove(&ele.id());
					}
					if pending.is_empty() {
						break;
					}
					remaining_views = pending;
				}
				for e in sorted.into_iter().rev() {
					Pg(e).drop(sql, rn);
				}
			}
		}

		let mut fkss = vec![];
		for ((diff, column_changes), constraint_changes) in
			diffs.iter().zip(changes.iter()).zip(fksd)
		{
			let fks = Pg(diff).print_stage2(
				sql,
				rn,
				column_changes,
				constraint_changes,
				diff.new.is_external(),
				report_old,
				report_new,
			);
			fkss.push(fks);
		}

		for (diff, column_changes) in diffs.iter().zip(changes) {
			Pg(diff).print_stage3(sql, rn, column_changes, diff.new.is_external());
		}

		// Create new views, in toposorted order
		{
			let mut remaining_views = changelist
				.created
				.iter()
				.filter_map(SchemaItem::as_view)
				.collect_vec();
			if !remaining_views.is_empty() {
				let mut dependencies = remaining_views
					.iter()
					.map(|c| c.id())
					.collect::<HashSet<_>>();
				loop {
					let (ready, pending) = remaining_views
						.iter()
						.partition::<Vec<SchemaView<'_>>, _>(|c| {
							for def in &c.definition.0 {
								match def {
									DefinitionPart::Raw(_) => {}
									DefinitionPart::TableRef(t) => {
										if dependencies.contains(&Ident::unchecked_cast(*t)) {
											return false;
										}
									}
									DefinitionPart::ColumnRef(t, _) => {
										if dependencies.contains(&Ident::unchecked_cast(*t)) {
											return false;
										}
									}
								}
							}
							true
						});
					if ready.is_empty() {
						let mut e =
							report_new.error("circular dependency while creating new views");
						for ele in pending {
							e.annotate("cycle part", ele.id().span());
						}
						break;
					}
					for ele in ready {
						Pg(ele).create(sql, rn, report_new);
						dependencies.remove(&ele.id());
					}
					if pending.is_empty() {
						break;
					}
					remaining_views = pending;
				}
			}
		}

		// Create new foreign keys
		assert_eq!(diffs.len(), fkss.len());
		for (diff, fks) in diffs.iter().zip(fkss) {
			Pg(diff.new).print_alternations(&fks, sql, rn);
		}
		for ele in changelist.created.iter().filter_map(SchemaItem::as_table) {
			let mut out = Vec::new();
			let ele = Pg(ele);
			let constraints = pg_constraints(&ele);
			for constraint in constraints {
				constraint.create_alter_fk(&mut out, rn);
			}
			ele.print_alternations(&out, sql, rn);
		}

		// Drop foreign keys, both ends of which reference dropped tables
		for a in changelist.dropped.iter().filter_map(SchemaItem::as_table) {
			let mut out = Vec::new();
			'fk: for fk in a.foreign_keys() {
				for b in changelist.dropped.iter().filter_map(SchemaItem::as_table) {
					if fk.target == b.id() {
						Pg(PgTableConstraint::ForeignKey(fk)).drop_alter_fk(&mut out, rn);
						continue 'fk;
					}
				}
			}
			Pg(a).print_alternations(&out, sql, rn);
		}

		// Drop old tables
		for ele in changelist.dropped.iter().filter_map(SchemaItem::as_table) {
			if ele.is_external() {
				continue;
			}
			Pg(ele).drop(sql, rn);
		}

		// Drop old types, in toposorted order
		{
			let mut remaining_types = changelist
				.dropped
				.iter()
				.filter_map(SchemaItem::as_type)
				.filter(|v| match v {
					// Do not drop external scalars
					SchemaType::Scalar(s) => !s.is_external(),
					_ => true,
				})
				.collect_vec();
			if !remaining_types.is_empty() {
				let mut sorted = vec![];
				let mut dependencies = remaining_types
					.iter()
					.map(|c| c.ident())
					.collect::<HashSet<_>>();
				loop {
					let (ready, pending) = remaining_types
						.iter()
						.partition::<Vec<SchemaType<'_>>, _>(|c| {
							let mut deps = Vec::new();
							c.type_dependencies(&mut deps);
							deps.iter().all(|f| !dependencies.contains(f))
						});
					if ready.is_empty() {
						let mut e =
							report_new.error("circular dependency while dropping old types");
						for ele in pending {
							e.annotate("cycle part", ele.ident().span());
						}
						break;
					}
					for ele in ready.into_iter().rev() {
						sorted.push(ele);
						dependencies.remove(&ele.ident());
					}
					if pending.is_empty() {
						break;
					}
					remaining_types = pending;
				}
				for e in sorted.into_iter().rev() {
					Pg(e).drop(sql, rn);
				}
			}
		};

		// Reconcile comments on schema items
		for ele in changelist.created {
			let id = Pg(ele).comment_id(rn);
			let new_c = Pg(ele).docs();

			if !new_c.is_empty() {
				let doc = new_c.replace('\'', "''");
				wl!(sql, "COMMENT ON {id} IS '{doc}';");
			}
		}
		for ele in changelist.updated {
			let id = Pg(ele.old).comment_id(rn);
			let old_c = Pg(ele.old).docs();
			let new_c = Pg(ele.new).docs();

			if old_c == new_c {
				continue;
			}
			if new_c.is_empty() {
				wl!(sql, "COMMENT ON {id} IS NULL;");
			} else {
				let doc = new_c.replace('\'', "''");
				wl!(sql, "COMMENT ON {id} IS '{doc}';");
			}
		}
	}
}
impl Pg<&Schema> {
	pub fn diff(
		&self,
		old: &Self,
		sql: &mut String,
		rn: &mut RenameMap,
		report_old: &mut Report,
		report_new: &mut Report,
	) {
		Pg(SchemaDiff { old, new: self }).print(sql, rn, report_old, report_new);
	}
	pub fn create(&self, sql: &mut String, rn: &mut RenameMap, report: &mut Report) {
		self.diff(&Pg(&Schema::default()), sql, rn, &mut Report::new(), report);
	}
	pub fn drop(&self, sql: &mut String, rn: &mut RenameMap, report: &mut Report) {
		Pg(&Schema::default()).diff(self, sql, rn, report, &mut Report::new());
	}
}
impl Pg<SchemaEnum<'_>> {
	pub fn rename(&self, db: DbType, sql: &mut String, rn: &mut RenameMap) {
		self.print_alternations(&[format!("RENAME TO {}", Id(&db))], sql, rn);
		self.set_db(rn, db);
	}
	pub fn create(&self, sql: &mut String, rn: &RenameMap) {
		let db_name = Id(self.db(rn));
		w!(sql, "CREATE TYPE {db_name} AS ENUM (\n");
		for (i, v) in self.items.iter().enumerate() {
			if i != 0 {
				w!(sql, ",");
			}
			// TODO: Escape as literal
			w!(sql, "\t'{}'\n", v.db(rn).raw());
		}
		wl!(sql, ");");
		wl!(sql,);
	}
	pub fn drop(&self, sql: &mut String, rn: &RenameMap) {
		let db_name = Id(self.db(rn));
		w!(sql, "DROP TYPE {db_name};\n");
	}
	pub fn print_alternations(&self, out: &[String], sql: &mut String, rn: &RenameMap) {
		if out.is_empty() {
			return;
		}
		let name = Id(self.db(rn));
		w!(sql, "ALTER TYPE {name}\n");
		for (i, alt) in out.iter().enumerate() {
			if i != 0 {
				w!(sql, ",");
			}
			wl!(sql, "\t{alt}");
		}
		wl!(sql, ";");
	}
}
impl Pg<EnumItemHandle<'_>> {
	pub fn rename_alter(&self, to: DbEnumItem, rn: &mut RenameMap) -> String {
		// TODO: Escape both as literals
		let out = format!("RENAME VALUE '{}' TO {}", self.db(rn).raw(), to.raw());
		self.set_db(rn, to);
		out
	}
}
impl Pg<EnumDiff<'_>> {
	pub fn print_renamed_added(
		&self,
		sql: &mut String,
		rn: &mut RenameMap,
		report_old: &mut Report,
		report_new: &mut Report,
	) {
		let changelist = schema::mk_change_list(
			rn,
			&self.0.old.items().collect_vec(),
			&self.0.new.items().collect_vec(),
			|v| v,
			report_old,
			report_new,
		);
		let mut changes = vec![];
		for el in changelist.renamed.iter().cloned() {
			let mut stored = HashMap::new();
			match el {
				RenameOp::Store(i, t) => {
					stored.insert(t, i);
					changes.push(Pg(i).rename_alter(t.db(), rn));
				}
				RenameOp::Moveaway(i, t) => {
					changes.push(Pg(i).rename_alter(t.db(), rn));
				}
				RenameOp::Rename(v, n, _) => {
					changes.push(Pg(v).rename_alter(n, rn));
				}
				RenameOp::Restore(r, n, _) => {
					let stored = stored.remove(&r).expect("was not stored");
					Pg(stored).rename_alter(n, rn);
				}
			}
		}

		for added in &changelist.created {
			// TODO: Escape literal
			changes.push(format!("ADD VALUE '{}'", added.db(rn).raw()));
		}

		Pg(self.old).print_alternations(&changes, sql, rn);
		assert!(
			changelist.dropped.is_empty(),
			"enums with dropped elements are not compatible"
		);
	}
}
impl Pg<SchemaComposite<'_>> {
	pub fn rename(&self, db: DbType, sql: &mut String, rn: &mut RenameMap) {
		self.print_alternations(&[format!("RENAME TO {}", Id(&db))], sql, rn);
		self.set_db(rn, db);
	}
	pub fn create(&self, sql: &mut String, rn: &RenameMap, report: &mut Report) {
		let db_name = &self.db(rn);
		w!(sql, "CREATE TYPE {} AS (\n", Id(db_name));
		for (i, v) in self.fields().enumerate() {
			if i != 0 {
				w!(sql, ",");
			}
			let db_name = Id(v.db(rn));
			let db_type = v.db_type(rn, report);
			w!(sql, "\t{db_name} {}\n", db_type.raw());
		}
		wl!(sql, ");");
	}
	pub fn drop(&self, sql: &mut String, rn: &RenameMap) {
		let db_name = Id(self.db(rn));
		w!(sql, "DROP TYPE {db_name};\n");
	}
	pub fn print_alternations(&self, out: &[String], sql: &mut String, rn: &RenameMap) {
		if out.is_empty() {
			return;
		}
		let name = Id(self.db(rn));
		w!(sql, "ALTER TYPE {name}\n");
		for (i, alt) in out.iter().enumerate() {
			if i != 0 {
				w!(sql, ",");
			}
			wl!(sql, "\t{alt}");
		}
		wl!(sql, ";");
	}
}
impl Pg<SchemaScalar<'_>> {
	pub fn rename(&self, to: DbType, sql: &mut String, rn: &mut RenameMap, external: bool) {
		if !external {
			self.print_alternations(&[format!("RENAME TO {}", Id(&to))], sql, rn);
		}
		self.scalar.set_db(rn, to);
	}
	pub fn print_alternations(&self, out: &[String], sql: &mut String, rn: &RenameMap) {
		if out.is_empty() {
			return;
		}
		let name = Id(self.db(rn));
		w!(sql, "ALTER DOMAIN {name}\n");
		for (i, alt) in out.iter().enumerate() {
			if i != 0 {
				w!(sql, ",");
			}
			wl!(sql, "\t{alt}");
		}
		wl!(sql, ";");
	}
	pub fn create(&self, sql: &mut String, rn: &RenameMap, report: &mut Report) {
		let name = Id(self.db(rn));
		let ty = self.inner_type(rn, report);
		w!(sql, "CREATE DOMAIN {name} AS {}", ty.raw());
		for ele in &self.annotations {
			match ele {
				ScalarAnnotation::Default(d) => {
					w!(sql, "\n\tDEFAULT ");
					let formatted =
						Pg(self.schema).format_sql(d, SchemaItem::Scalar(self.0), rn, report);
					w!(sql, "{formatted}");
				}
				ScalarAnnotation::Check(check) => {
					let name = Id(check.db(rn));
					w!(sql, "\n\tCONSTRAINT {name} CHECK (");
					let formatted = Pg(self.schema).format_sql(
						&check.check,
						SchemaItem::Scalar(self.0),
						rn,
						report,
					);
					w!(sql, "{formatted})");
				}
				ScalarAnnotation::Inline | ScalarAnnotation::External => {
					unreachable!("non-material scalars are not created")
				}
				ScalarAnnotation::Index(_)
				| ScalarAnnotation::Unique(_)
				| ScalarAnnotation::PrimaryKey(_) => {
					unreachable!("only check constrains are allowed on domain scalars")
				}
			}
		}
		wl!(sql, ";");
	}
	pub fn drop(&self, sql: &mut String, rn: &RenameMap) {
		assert!(!self.is_external(), "should not drop external scalars");
		let name = Id(self.db(rn));
		wl!(sql, "DROP DOMAIN {name};");
	}
}
impl Pg<SchemaType<'_>> {
	fn create(self, sql: &mut String, rn: &RenameMap, report: &mut Report) {
		match self.0 {
			SchemaType::Enum(e) => Pg(e).create(sql, rn),
			SchemaType::Scalar(s) => Pg(s).create(sql, rn, report),
			SchemaType::Composite(c) => Pg(c).create(sql, rn, report),
		}
	}
	fn drop(self, sql: &mut String, rn: &RenameMap) {
		match self.0 {
			SchemaType::Enum(e) => Pg(e).drop(sql, rn),
			SchemaType::Scalar(s) => Pg(s).drop(sql, rn),
			SchemaType::Composite(c) => Pg(c).drop(sql, rn),
		}
	}
}

impl Pg<&Check> {
	fn rename_alter(&self, to: DbConstraint, out: &mut Vec<String>, rn: &mut RenameMap) {
		let db = Id(self.db(rn));
		let to = Id(to);
		if db == to {
			return;
		}
		out.push(format!("ALTER CONSTRAINT {db} RENAME TO {to}"));
		self.set_db(rn, to.0);
	}
}
impl Pg<Diff<SchemaScalar<'_>>> {
	pub fn print(
		&self,
		sql: &mut String,
		rn: &mut RenameMap,
		external: bool,
		report_old: &mut Report,
		report_new: &mut Report,
	) {
		let mut new = self
			.new
			.annotations
			.iter()
			.filter_map(ScalarAnnotation::as_check)
			.map(|c| {
				let mut sql = String::new();
				Pg(self.new.schema.sql(&c.check)).print(
					&mut sql,
					SchemaItem::Scalar(self.new),
					rn,
					report_new,
				);
				(c, sql)
			})
			.collect::<Vec<_>>();
		let old = self
			.old
			.annotations
			.iter()
			.filter_map(ScalarAnnotation::as_check)
			.collect::<Vec<_>>();
		let mut out = Vec::new();
		for ann in &old {
			let mut sql = String::new();
			Pg(self.old.schema.sql(&ann.check)).print(
				&mut sql,
				SchemaItem::Scalar(self.old),
				rn,
				report_old,
			);

			if let Some((i, (c, _))) = new.iter().find_position(|(_, nsql)| nsql == &sql) {
				Pg(*ann).rename_alter(c.db(rn), &mut out, rn);
				new.remove(i);
			} else {
				let db = Id(ann.db(rn));
				out.push(format!("DROP CONSTRAINT {db}"));
			}
		}

		for (check, _) in &new {
			let db = Id(check.db(rn));
			let mut sql = format!("ADD CONSTRAINT {db} CHECK (");
			Pg(self.new.schema.sql(&check.check)).print(
				&mut sql,
				SchemaItem::Scalar(self.new),
				rn,
				report_new,
			);
			w!(sql, ")");
			out.push(sql);
		}

		if !external {
			Pg(self.old).print_alternations(&out, sql, rn);
		}
	}
}

fn cleanup_docs(mut docs: Vec<String>) -> Vec<String> {
	if docs.iter().all(|v| v.starts_with(' ')) {
		for ele in docs.iter_mut() {
			ele.remove(0);
			*ele = ele.trim_end().to_owned();
		}
	}
	while matches!(docs.first(), Some(v) if v.is_empty()) {
		docs.remove(0);
	}
	while matches!(docs.last(), Some(v) if v.is_empty()) {
		docs.pop();
	}
	docs
}
fn wrap_docs(mut docs: Vec<String>, header: String) -> Vec<String> {
	for ele in docs.iter_mut() {
		ele.insert_str(0, "    ");
	}
	docs.insert(0, header);
	docs
}
fn docs_to_string(mut docs: Vec<String>) -> String {
	docs = cleanup_docs(docs);
	docs.join("\n")
}

impl Pg<SchemaItem<'_>> {
	pub fn rename(&self, to: DbItem, sql: &mut String, rn: &mut RenameMap, external: bool) {
		match self.0 {
			SchemaItem::Table(t) => Pg(t).rename(DbTable::unchecked_from(to), sql, rn, external),
			SchemaItem::Enum(e) => Pg(e).rename(DbType::unchecked_from(to), sql, rn),
			SchemaItem::Scalar(s) => Pg(s).rename(DbType::unchecked_from(to), sql, rn, external),
			SchemaItem::Composite(c) => Pg(c).rename(DbType::unchecked_from(to), sql, rn),
			SchemaItem::View(v) => Pg(v).rename(DbView::unchecked_from(to), sql, rn),
		}
	}
	pub fn create(&self, sql: &mut String, rn: &RenameMap, report: &mut Report) {
		match self.0 {
			SchemaItem::Table(t) => Pg(t).create(sql, rn, report),
			SchemaItem::Enum(e) => Pg(e).create(sql, rn),
			SchemaItem::Scalar(s) => Pg(s).create(sql, rn, report),
			SchemaItem::Composite(c) => Pg(c).create(sql, rn, report),
			SchemaItem::View(_) => todo!(),
		}
	}
	pub fn drop(&self, sql: &mut String, rn: &RenameMap) {
		match self.0 {
			SchemaItem::Table(t) => Pg(t).drop(sql, rn),
			SchemaItem::Enum(e) => Pg(e).drop(sql, rn),
			SchemaItem::Scalar(s) => Pg(s).drop(sql, rn),
			SchemaItem::Composite(c) => Pg(c).drop(sql, rn),
			SchemaItem::View(c) => Pg(c).drop(sql, rn),
		}
	}
	pub fn docs(&self) -> String {
		let docs = match self.0 {
			SchemaItem::Table(t) => t.docs.clone(),
			SchemaItem::Enum(e) => {
				let mut docs = cleanup_docs(e.docs.clone());
				// Postgres enums have no way to associate comment with value, unfortunately.
				for ele in e.items() {
					let edocs = cleanup_docs(ele.docs.clone());
					if !edocs.is_empty() {
						let name = ele.id().name();
						let edocs = wrap_docs(edocs, format!("Value {name}:"));
						docs.push(String::new());
						docs.extend(edocs);
					}
				}
				docs
			}
			SchemaItem::Scalar(s) => s.docs.clone(),
			SchemaItem::Composite(c) => {
				let mut docs = cleanup_docs(c.docs.clone());
				// Postgres composites have no way to associate comment with value, unfortunately.
				for ele in c.fields() {
					let edocs = cleanup_docs(ele.docs.clone());
					if !edocs.is_empty() {
						let name = ele.id().name();
						let edocs = wrap_docs(edocs, format!("Field {name}:"));
						docs.push(String::new());
						docs.extend(edocs);
					}
				}
				docs
			}
			SchemaItem::View(v) => v.docs.clone(),
		};
		docs_to_string(docs)
	}
	pub fn comment_id(&self, rn: &RenameMap) -> String {
		let name = Id(self.db(rn));
		match self.0 {
			SchemaItem::Table(_) => format!("TABLE {name}"),
			SchemaItem::Scalar(_) => format!("DOMAIN {name}"),
			SchemaItem::Enum(_) | SchemaItem::Composite(_) => format!("TYPE {name}"),
			SchemaItem::View(_) => format!("VIEW {name}"),
		}
	}
}

fn sql_needs_parens(sql: &Sql, parent_binop: Option<SqlOp>) -> bool {
	match sql {
		Sql::Cast(_, _)
		| Sql::Call(_, _)
		| Sql::String(_)
		| Sql::Number(_)
		| Sql::Ident(_)
		| Sql::Parened(_)
		| Sql::Boolean(_)
		| Sql::GetField(_, _)
		| Sql::Placeholder
		| Sql::Tuple(_)
		| Sql::Null => false,

		// It is possible to fully implement precedence climbing here, however I have tried, and it resulted in
		// not very good looking code in some cases (Mix&matching LIKE, AND & IN); So I will only add variants, which should never conflict with what user sees.
		Sql::BinOp(_, a @ (SqlOp::And | SqlOp::Or), _) if Some(*a) == parent_binop => false,
		Sql::BinOp(_, SqlOp::And, _) if matches!(parent_binop, Some(SqlOp::Or)) => false,
		Sql::BinOp(
			_,
			SqlOp::Lt | SqlOp::Gt | SqlOp::Le | SqlOp::Ge | SqlOp::Ne | SqlOp::SEq | SqlOp::SNe,
			_,
		) if matches!(parent_binop, Some(SqlOp::And | SqlOp::Or)) => false,

		Sql::UnOp(_, _) | Sql::BinOp(_, _, _) | Sql::If(_, _, _) => true,
	}
}
fn format_sql(
	sql: &Sql,
	schema: &Schema,
	context: SchemaItem<'_>,
	rn: &RenameMap,
	report: &mut Report,
) -> String {
	let mut out = String::new();
	match sql {
		Sql::Cast(expr, ty) => {
			let expr = format_sql(expr, schema, context, rn, report);
			let native_ty = Id(schema.native_type(ty, rn, report));
			w!(out, "({expr})::{native_ty}");
		}
		Sql::Call(proc, args) => {
			let proc = Id(proc);
			w!(out, "{proc}(");
			for (i, arg) in args.iter().enumerate() {
				if i != 0 {
					w!(out, ", ");
				}
				let arg = format_sql(arg, schema, context, rn, report);
				w!(out, "{arg}");
			}
			w!(out, ")");
		}
		Sql::String(s) => {
			w!(out, "'{s}'");
		}
		Sql::Number(n) => {
			w!(out, "{n}");
		}
		Sql::Ident(_) => {
			if let Some(native_name) = sql.ident_name(&context, rn, report) {
				let native_name = Id(native_name);
				w!(out, "{native_name}");
			} else {
				w!(out, "ERROR")
			}
		}
		Sql::UnOp(op, expr) => {
			let op = op.format();
			let expr = format_sql(expr, schema, context, rn, report);
			w!(out, "{op}({expr})");
		}
		Sql::BinOp(a, op, b) => {
			let sop = op.format();
			let va = format_sql(a, schema, context, rn, report);
			let vb = format_sql(b, schema, context, rn, report);
			if sql_needs_parens(a, Some(*op)) {
				w!(out, "({va})");
			} else {
				w!(out, "{va}");
			}
			match (op, vb.as_str()) {
				// String comparison is not looking good, but it works...
				(SqlOp::SEq, "NULL") => w!(out, " IS NULL"),
				(SqlOp::SNe, "NULL") => w!(out, " IS NOT NULL"),
				_ => {
					w!(out, " {sop} ");
					if sql_needs_parens(b, Some(*op)) {
						w!(out, "({vb})");
					} else {
						w!(out, "{vb}");
					}
				}
			}
		}
		Sql::Parened(a) => {
			let va = format_sql(a, schema, context, rn, report);
			if sql_needs_parens(a, None) {
				w!(out, "({va})");
			} else {
				w!(out, "{va}");
			}
		}
		Sql::Boolean(b) => {
			if *b {
				w!(out, "TRUE");
			} else {
				w!(out, "FALSE");
			}
		}
		Sql::Placeholder => {
			match context {
				SchemaItem::Table(_) | SchemaItem::View(_) => {
					unreachable!("placeholder should be replaced on this point")
				}
				SchemaItem::Enum(_) => panic!("enums have no sql items"),
				SchemaItem::Scalar(_) => w!(out, "VALUE"),
				SchemaItem::Composite(_) => panic!("composite checks should be inlined"),
			};
		}
		Sql::Null => w!(out, "NULL"),
		Sql::GetField(f, c) => {
			let va = format_sql(f, schema, context, rn, report);
			if let Some(id) = f.field_name(&context, *c, rn, report) {
				let name = Id(id);
				if sql_needs_parens(f, None) {
					w!(out, "({va})")
				} else {
					w!(out, "{va}");
				}
				w!(out, ".{name}");
			} else {
				assert!(report.is_error());
				w!(out, "{va}(ERROR)")
			}
		}
		Sql::Tuple(t) => {
			w!(out, "ROW(");
			for (i, v) in t.iter().enumerate() {
				if i != 0 {
					w!(out, ", ");
				}
				let va = format_sql(v, schema, context, rn, report);
				w!(out, "{va}");
			}
			w!(out, ")");
		}
		Sql::If(cond, then, else_) => {
			let cond = format_sql(cond, schema, context, rn, report);
			let then = format_sql(then, schema, context, rn, report);
			let else_ = format_sql(else_, schema, context, rn, report);
			w!(out, "CASE WHEN ({cond}) THEN ({then}) ELSE ({else_}) END");
		}
	}
	out
}

impl Pg<SchemaSql<'_>> {
	pub fn print(
		&self,
		sql: &mut String,
		context: SchemaItem<'_>,
		rn: &RenameMap,
		report: &mut Report,
	) {
		let o = format_sql(&self.0, self.schema, context, rn, report);
		sql.push_str(&o);
	}
}
impl Pg<&Schema> {
	pub fn format_sql(
		&self,
		sql: &Sql,
		context: SchemaItem<'_>,
		rn: &RenameMap,
		report: &mut Report,
	) -> String {
		let mut out = String::new();
		Pg(self.sql(sql)).print(&mut out, context, rn, report);
		out
	}
}

#[derive(Clone, Copy, Debug)]
pub enum PgTableConstraint<'s> {
	PrimaryKey(TablePrimaryKey<'s>),
	Unique(TableUniqueConstraint<'s>),
	Check(TableCheck<'s>),
	ForeignKey(TableForeignKey<'s>),
}
impl HasUid for PgTableConstraint<'_> {
	fn uid(&self) -> schema::uid::Uid {
		match self {
			PgTableConstraint::PrimaryKey(p) => p.uid(),
			PgTableConstraint::Unique(u) => u.uid(),
			PgTableConstraint::Check(c) => c.uid(),
			PgTableConstraint::ForeignKey(f) => f.uid(),
		}
	}
}
impl HasDefaultDbName for PgTableConstraint<'_> {
	type Kind = ConstraintKind;

	fn default_db(&self) -> Option<DbIdent<Self::Kind>> {
		match self {
			PgTableConstraint::PrimaryKey(p) => p.default_db(),
			PgTableConstraint::Unique(u) => u.default_db(),
			PgTableConstraint::Check(c) => c.default_db(),
			PgTableConstraint::ForeignKey(f) => f.default_db().map(DbIdent::unchecked_from),
		}
	}
}
impl IsIsomorph for PgTableConstraint<'_> {
	fn is_isomorph(
		&self,
		other: &Self,
		rn: &RenameMap,
		report_self: &mut Report,
		report_other: &mut Report,
	) -> bool {
		match (self, other) {
			(PgTableConstraint::PrimaryKey(_), PgTableConstraint::PrimaryKey(_)) => {
				// There is only one pk per table, its just makes sense
				true
			}
			(PgTableConstraint::Unique(a), PgTableConstraint::Unique(b)) => {
				let mut a_columns = a.table.db_names(a.columns.clone(), rn);
				a_columns.sort();
				let mut b_columns = b.table.db_names(b.columns.clone(), rn);
				b_columns.sort();
				// It makes little sense to have multiple unique constraints with the same set of columns, except if it used as implicit index.
				// Anyway, lets try to recreate/rename that.
				a.db(rn) == b.db(rn) || a_columns == b_columns
			}
			(PgTableConstraint::Check(a), PgTableConstraint::Check(b)) => {
				let mut sql_a = String::new();
				Pg(a.table.sql(&a.check)).print(&mut sql_a, rn, report_self);
				let mut sql_b = String::new();
				Pg(b.table.sql(&b.check)).print(&mut sql_b, rn, report_other);
				a.db(rn) == b.db(rn) || sql_a == sql_b
			}
			(PgTableConstraint::ForeignKey(a), PgTableConstraint::ForeignKey(b)) => {
				if a.db(rn) == b.db(rn) {
					return true;
				}
				if a.target_table().db(rn) != b.target_table().db(rn) {
					return false;
				}
				let mut a_source_columns = a.source_db_columns(rn);
				let mut a_target_columns = a.target_db_columns(rn);
				let mut b_source_columns = b.source_db_columns(rn);
				let mut b_target_columns = b.target_db_columns(rn);
				let mut a_perm_by_source = permutation::sort(&a_source_columns);
				a_perm_by_source.apply_slice_in_place(&mut a_source_columns);
				a_perm_by_source.apply_slice_in_place(&mut a_target_columns);
				let mut b_perm_by_source = permutation::sort(&b_source_columns);
				b_perm_by_source.apply_slice_in_place(&mut b_source_columns);
				b_perm_by_source.apply_slice_in_place(&mut b_target_columns);
				if a_source_columns == b_source_columns && a_target_columns == b_target_columns {
					return true;
				}

				let mut a_perm_by_target = permutation::sort(&a_target_columns);
				a_perm_by_target.apply_slice_in_place(&mut a_source_columns);
				a_perm_by_target.apply_slice_in_place(&mut a_target_columns);
				let mut b_perm_by_target = permutation::sort(&b_target_columns);
				b_perm_by_target.apply_slice_in_place(&mut b_source_columns);
				b_perm_by_target.apply_slice_in_place(&mut b_target_columns);
				if a_source_columns == b_source_columns && a_target_columns == b_target_columns {
					return true;
				}

				false
			}
			_ => false,
		}
	}
}
// Constraints are not updateable, except for foreign keys... But they are kinda special.
impl IsCompatible for PgTableConstraint<'_> {
	fn is_compatible(
		&self,
		new: &Self,
		rn: &RenameMap,
		report_self: &mut Report,
		report_new: &mut Report,
	) -> bool {
		match (self, new) {
			(PgTableConstraint::PrimaryKey(a), PgTableConstraint::PrimaryKey(b)) => {
				let a_columns = a.table.db_names(a.columns.clone(), rn);
				let b_columns = b.table.db_names(b.columns.clone(), rn);
				a_columns == b_columns
			}
			(PgTableConstraint::Unique(a), PgTableConstraint::Unique(b)) => {
				let a_columns = a.table.db_names(a.columns.clone(), rn);
				let b_columns = b.table.db_names(b.columns.clone(), rn);
				a_columns == b_columns
			}
			(PgTableConstraint::Check(a), PgTableConstraint::Check(b)) => {
				let mut sql_a = String::new();
				Pg(a.table.sql(&a.check)).print(&mut sql_a, rn, report_self);
				let mut sql_b = String::new();
				Pg(b.table.sql(&b.check)).print(&mut sql_b, rn, report_new);
				sql_a == sql_b
			}
			(PgTableConstraint::ForeignKey(a), PgTableConstraint::ForeignKey(b)) => {
				assert_eq!(
					a.target_table().db(rn),
					b.target_table().db(rn),
					"rejected by isomorp test"
				);
				if a.on_delete != b.on_delete {
					return false;
				}
				let a_source_columns = a.source_db_columns(rn);
				let b_source_columns = b.source_db_columns(rn);
				let a_target_columns = a.target_db_columns(rn);
				let b_target_columns = b.target_db_columns(rn);
				a_source_columns == b_source_columns && a_target_columns == b_target_columns
			}
			_ => unreachable!("non-isomorphs are rejected"),
		}
	}
}
impl PgTableConstraint<'_> {
	fn is_fk(&self) -> bool {
		matches!(self, Self::ForeignKey(_))
	}
	fn drop_alter_non_fk(&self, out: &mut Vec<Alternation>, rn: &RenameMap) {
		if self.is_fk() {
			return;
		}
		let name = Id(self.db(rn));
		out.push(alt_group!("DROP CONSTRAINT {name}"));
	}
	fn drop_alter_fk(&self, out: &mut Vec<Alternation>, rn: &RenameMap) {
		if !self.is_fk() {
			return;
		}
		let name = Id(self.db(rn));
		out.push(alt_group!("DROP CONSTRAINT {name}"));
	}
	pub fn rename_alter(
		&self,
		new_name: DbConstraint,
		out: &mut Vec<Alternation>,
		rn: &mut RenameMap,
	) {
		let name = Id(self.db(rn));
		let new_name = Id(new_name);
		if name == new_name {
			return;
		}
		out.push(alt_ungroup!("RENAME CONSTRAINT {name} TO {new_name}"));
		self.set_db(rn, new_name.0);
	}
	pub fn create_alter_non_fk(
		&self,
		out: &mut Vec<Alternation>,
		rn: &RenameMap,
		report: &mut Report,
	) {
		let text = match self {
			PgTableConstraint::PrimaryKey(p) => Pg(*p).create_inline(rn),
			PgTableConstraint::Unique(u) => Pg(*u).create_inline(rn),
			PgTableConstraint::Check(c) => Pg(*c).create_inline(rn, report),
			PgTableConstraint::ForeignKey(_) => return,
		};
		out.push(alt_group!("ADD {text}"));
	}
	pub fn create_alter_fk(&self, out: &mut Vec<Alternation>, rn: &RenameMap) {
		if let PgTableConstraint::ForeignKey(f) = self {
			Pg(*f).create_alter(out, rn);
		};
	}
	pub fn create_inline_non_fk(&self, rn: &RenameMap, report: &mut Report) -> Option<String> {
		Some(match self {
			PgTableConstraint::PrimaryKey(pk) => Pg(*pk).create_inline(rn),
			PgTableConstraint::Unique(u) => Pg(*u).create_inline(rn),
			PgTableConstraint::Check(c) => Pg(*c).create_inline(rn, report),
			PgTableConstraint::ForeignKey(_) => return None,
		})
	}
}

impl Pg<TablePrimaryKey<'_>> {
	pub fn create_inline(&self, rn: &RenameMap) -> String {
		let mut sql = String::new();
		w!(sql, "CONSTRAINT {} PRIMARY KEY(", Id(self.db(rn)));
		self.table
			.print_column_list(&mut sql, self.columns.iter().copied(), rn);
		w!(sql, ")");
		sql
	}
	pub fn drop_alter(&self, out: &mut Vec<String>, rn: &RenameMap) {
		out.push(format!("DROP CONSTRAINT {}", Id(self.db(rn))));
	}
	pub fn rename_alter(&self, new_name: DbConstraint, out: &mut Vec<String>, rn: &mut RenameMap) {
		out.push(format!(
			"RENAME CONSTRAINT {} TO {}",
			Id(self.db(rn)),
			Id(&new_name)
		));
		self.set_db(rn, new_name);
	}
}
impl Pg<TableUniqueConstraint<'_>> {
	pub fn create_inline(&self, rn: &RenameMap) -> String {
		let mut sql = String::new();
		w!(sql, "CONSTRAINT {} UNIQUE(", Id(self.db(rn)));
		self.table
			.print_column_list(&mut sql, self.columns.iter().copied(), rn);
		wl!(sql, ")");
		sql
	}
	pub fn drop_alter(&self, out: &mut Vec<String>, rn: &RenameMap) {
		out.push(format!("DROP CONSTRAINT {}", Id(self.db(rn))));
	}
}
impl Pg<TableCheck<'_>> {
	pub fn create_inline(&self, rn: &RenameMap, report: &mut Report) -> String {
		let mut sql = String::new();
		w!(sql, "CONSTRAINT {} CHECK (", Id(self.db(rn)));
		Pg(self.table.sql(&self.check)).print(&mut sql, rn, report);
		w!(sql, ")");
		sql
	}
	pub fn drop_alter(&self, out: &mut Vec<String>, rn: &RenameMap) {
		out.push(format!("DROP CONSTRAINT {}", Id(self.db(rn))));
	}
}

impl Pg<SchemaView<'_>> {
	pub fn create(&self, sql: &mut String, rn: &RenameMap, report: &mut Report) {
		let table_name = Id(self.db(rn));
		w!(sql, "CREATE");
		if self.materialized {
			w!(sql, " MATERIALIZED");
		}
		w!(sql, " VIEW {table_name} AS");
		for ele in &self.0.definition.0 {
			match ele {
				DefinitionPart::Raw(r) => {
					w!(sql, "{r}")
				}
				DefinitionPart::TableRef(t) => {
					if let Some(table) = self.schema.schema_table_or_view(t) {
						match table {
							SchemaTableOrView::Table(t) => {
								w!(sql, "{}", Id(t.db(rn)))
							}
							SchemaTableOrView::View(v) => {
								w!(sql, "{}", Id(v.db(rn)))
							}
						}
					} else {
						report
							.error("referenced table not found")
							.annotate("referenced here", t.span());
						w!(sql, "ERROR")
					}
				}
				DefinitionPart::ColumnRef(t, c) => {
					if let Some(table) = self.schema.schema_table_or_view(t) {
						if let SchemaTableOrView::Table(table) = table {
							if let Some(db) =
								Sql::context_ident_name(&SchemaItem::Table(table), *c, rn, report)
							{
								let db = Id(db);
								w!(sql, "{db}")
							} else {
								w!(sql, "ERROR")
							}
						} else {
							report
								.error("column references only work on tables, not on views")
								.annotate("referenced here", c.span());
							w!(sql, "ERROR")
						}
					} else {
						report
							.error("referenced table not found")
							.annotate("referenced here", t.span());
						w!(sql, "ERROR")
					}
				}
			}
		}
		wl!(sql, ";");
	}
	pub fn rename(&self, to: DbView, sql: &mut String, rn: &mut RenameMap) {
		self.print_alternations(&[alt_ungroup!("RENAME TO {}", Id(&to))], sql, rn);
		self.set_db(rn, to);
	}
	pub fn drop(&self, sql: &mut String, rn: &RenameMap) {
		let name = Id(self.db(rn));
		w!(sql, "DROP");
		if self.materialized {
			w!(sql, " MATERIALIZED");
		}
		wl!(sql, " VIEW {name};");
	}

	pub fn print_alternations(&self, mut out: &[Alternation], sql: &mut String, rn: &RenameMap) {
		fn print_group(materialized: bool, name: &DbView, list: &[Alternation], sql: &mut String) {
			let name = Id(name);
			w!(sql, "ALTER");
			if materialized {
				w!(sql, " MATERIALIZED");
			}
			if list.len() > 1 {
				w!(sql, " VIEW {name}\n");
				for (i, alt) in list.iter().enumerate() {
					if i != 0 {
						w!(sql, ",");
					};
					wl!(sql, "\t{}", alt.alt);
				}
				wl!(sql, ";");
			} else {
				let alt = &list[0];
				w!(sql, " VIEW {name} {};\n", alt.alt);
			}
		}
		let name = &self.db(rn);
		while !out.is_empty() {
			let mut count = 1;
			loop {
				if !out[count - 1].groupable_down || out.len() == count {
					break;
				}
				if !out[count].groupable_up {
					break;
				}
				count += 1;
			}
			print_group(self.materialized, name, &out[..count], sql);
			out = &out[count..];
		}
	}
}

#[derive(PartialEq, Clone, Copy)]
struct Id<T>(T);
impl<T> Display for Id<DbIdent<T>> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let id = escape_identifier(self.0.raw());
		write!(f, "{id}")
	}
}
impl<T> Display for Id<&DbIdent<T>> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let id = escape_identifier(self.0.raw());
		write!(f, "{id}")
	}
}
impl Display for Id<&str> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let id = escape_identifier(self.0);
		write!(f, "{id}")
	}
}

#[derive(PartialEq, Eq)]
struct FingerprintBuilder(Vec<u8>);
impl FingerprintBuilder {
	fn new() -> Self {
		let mut value = FingerprintBuilder(Vec::new());
		value.section("start");
		value
	}
	fn section(&mut self, name: impl AsRef<[u8]>) {
		self.raw(0, name);
	}
	fn fragment(&mut self, fragment: impl AsRef<[u8]>) {
		self.raw(1, fragment);
	}
	fn raw(&mut self, kind: u8, raw: impl AsRef<[u8]>) {
		self.0.push(kind);
		let raw = raw.as_ref();
		let encoded = u32::to_be_bytes(raw.len() as u32);
		self.0.extend(&encoded);
		self.0.extend(raw);
	}
	fn sub(&mut self, name: impl AsRef<[u8]>, sub: FingerprintBuilder) {
		self.section(name);
		self.0.extend(sub.0);
		self.section("[END]");
	}
}

impl IsCompatible for Pg<SchemaItem<'_>> {
	fn is_compatible(
		&self,
		new: &Self,
		rn: &RenameMap,
		report_self: &mut Report,
		report_new: &mut Report,
	) -> bool {
		match (self.0, new.0) {
			(SchemaItem::Table(_), SchemaItem::Table(_)) => true,
			(SchemaItem::Enum(a), SchemaItem::Enum(b)) => {
				// There is no DB engine, which supports removing enum variants, so removals are incompatible, and the
				// enum should be recreated.
				mk_change_list(
					rn,
					&a.items().collect_vec(),
					&b.items().collect_vec(),
					|v| v,
					report_self,
					report_new,
				)
				.dropped
				.is_empty()
			}
			(SchemaItem::Composite(a), SchemaItem::Composite(b)) => {
				// There is no DB engine, which supports updating structs
				let changes = mk_change_list(
					rn,
					&a.fields().collect_vec(),
					&b.fields().collect_vec(),
					|v| v,
					report_self,
					report_new,
				);
				changes.dropped.is_empty() && changes.created.is_empty()
			}
			(SchemaItem::Scalar(a), SchemaItem::Scalar(b)) => {
				if a.is_external() || b.is_external() {
					return true;
				}
				a.inner_type(rn, report_self) == b.inner_type(rn, report_new)
			}
			(SchemaItem::View(a), SchemaItem::View(b)) => {
				fn fingerprint(
					view: SchemaView<'_>,
					rn: &RenameMap,
					report: &mut Report,
				) -> FingerprintBuilder {
					let mut fp = FingerprintBuilder::new();
					for part in &view.definition.0 {
						match part {
							DefinitionPart::Raw(r) => {
								fp.section("raw");
								fp.fragment(r);
							}
							DefinitionPart::TableRef(t) => {
								let t = view.schema.schema_table_or_view(t).expect("exists");
								match t {
									SchemaTableOrView::Table(t) => {
										fp.section("subtable");
										for ele in t.columns() {
											fp.section("name");
											fp.fragment(ele.db(rn).raw());
											fp.section("ty");
											fp.fragment(ele.db_type(rn, report).raw());
										}
									}
									SchemaTableOrView::View(v) => {
										fp.sub("reqview", fingerprint(v, rn, report));
									}
								}
							}
							DefinitionPart::ColumnRef(t, c) => {
								let t = view.schema.schema_table_or_view(t).expect("exists");
								match t {
									SchemaTableOrView::Table(t) => {
										fp.section("subtable");
										let c = t.schema_column(*c);
										fp.section("name");
										fp.fragment(c.db(rn).raw());
										fp.section("ty");
										fp.fragment(c.db_type(rn, report).raw());
									}
									SchemaTableOrView::View(v) => {
										// If view is rebuilt, current view can't be the same.
										fp.sub("reqview", fingerprint(v, rn, report));
									}
								}
							}
						}
					}
					fp
				}
				if a.materialized != b.materialized {
					return false;
				}
				let fpa = fingerprint(a, rn, report_self);
				let fpb = fingerprint(b, rn, report_new);
				fpa == fpb
			}
			_ => false,
		}
		// matches!(
		// 	(self, new),
		// 	(Self::Table(_), Self::Table(_))
		// 		| (Self::Enum(a), Self::Enum(_))
		// 		| (Self::Scalar(_), Self::Scalar(_))
		// )
	}
}
impl IsIsomorph for Pg<SchemaItem<'_>> {
	fn is_isomorph(
		&self,
		other: &Self,
		rn: &RenameMap,
		report_self: &mut Report,
		report_other: &mut Report,
	) -> bool {
		self.0.is_isomorph(&other.0, rn, report_self, report_other)
	}
}
impl Pg<SchemaDiff<'_>> {
	pub fn changelist(
		&self,
		rn: &RenameMap,
		report_old: &mut Report,
		report_new: &mut Report,
	) -> ChangeList<SchemaItem<'_>> {
		let old = self.old.material_items();
		let new = self.new.material_items();
		mk_change_list(
			rn,
			old.as_slice(),
			new.as_slice(),
			Pg,
			report_old,
			report_new,
		)
	}
}

#[cfg(test)]
mod tests {
	use std::{fs, io::Write, path::PathBuf};

	use schema::diagnostics::Report;
	use schema::{
		Diff, parser::parse, process::NamingConvention, root::SchemaProcessOptions, uid::RenameMap,
		wl,
	};
	use tempfile::NamedTempFile;
	use tracing_test::traced_test;

	use crate::Pg;

	pub fn default_options() -> SchemaProcessOptions {
		SchemaProcessOptions {
			generator_supports_domain: true,
			naming_convention: NamingConvention::Postgres,
		}
	}

	fn test_example(name: &str) {
		#[derive(Debug)]
		struct Update {
			description: String,
			schema: String,
		}

		let mut data = fs::read_to_string(name).expect("example read");
		let result_offset = data.find("\n!!!RESULT\n").unwrap_or(data.len());
		data.truncate(result_offset);
		let (defaults, parts) = data.split_once("!!!TEST").unwrap_or(("", &data));

		let defaults = defaults.strip_prefix("!!!SETUP\n").unwrap_or(defaults);

		let mut examples = parts
			.split("\n!!!UPDATE")
			.map(|s| {
				let (description, text) = s.split_once('\n').unwrap_or((s, ""));
				Update {
					description: description.trim().to_string(),
					schema: text.to_string(),
				}
			})
			.collect::<Vec<_>>();
		examples.push(Update {
			description: "cleanup schema changes".to_owned(),
			schema: String::new(),
		});
		if !defaults.is_empty() {
			for example in &mut examples {
				if !example.schema.is_empty() {
					example.schema.insert(0, '\n');
				}
				example.schema.insert_str(0, defaults);
			}
			examples.insert(
				0,
				Update {
					description: "setup".to_owned(),
					schema: defaults.to_owned(),
				},
			);
		}
		// Init defaults
		examples.insert(
			0,
			Update {
				description: "in the beginning there was nothing (doesn't exist in output)"
					.to_owned(),
				schema: String::new(),
			},
		);
		// Drop defaults
		if !defaults.is_empty() {
			examples.push(Update {
				description: "cleanup setup".to_owned(),
				schema: String::new(),
			});
		}
		let mut rn = RenameMap::default();
		let examples = examples
			.iter()
			.map(|example| {
				let mut report = Report::new();
				let schema = match parse(
					example.schema.as_str(),
					false,
					&default_options(),
					&mut rn,
					&mut report,
				) {
					Ok(s) => s,
					Err(e) => {
						panic!()
						// for e in &e {
						// 	match e {
						// 		schema::parser::ParsingError::Peg(e) => {
						// 			eprintln!(
						// 				"buffer start: {}",
						// 				&example.schema.as_str()[e.location.offset..]
						// 			);
						// 		}
						// 	}
						// }
						// panic!("failed to parse schema:\n{}\n\n{e:#?}", example.schema);
					}
				};
				if report.is_error() {
					panic!("error found in report");
				}
				(example.description.clone(), schema)
			})
			.collect::<Vec<_>>();
		let mut out = String::new();
		for ele in examples.windows(2) {
			let description = &ele[1].0;
			if description.is_empty() {
				wl!(out, "\n-- updated --");
			} else {
				wl!(out, "\n-- updated: {description} --");
			}
			let mut out_tmp = String::new();
			Pg(Diff {
				old: &ele[0].1,
				new: &ele[1].1,
			})
			.print(
				&mut out_tmp,
				&mut rn,
				&mut Report::new(),
				&mut Report::new(),
			);
			out.push_str(out_tmp.trim());
		}
		let output = format!("{}\n!!!RESULT\n{}\n", data.trim(), out.trim());
		let mut named_file =
			NamedTempFile::new_in(PathBuf::from(name).parent().expect("parent")).expect("new temp");
		named_file.write_all(output.as_bytes()).expect("write");
		named_file.persist(name).expect("persist");
	}
	include!(concat!(env!("OUT_DIR"), "/example_tests.rs"));
}
