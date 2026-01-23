use std::collections::BTreeSet;

use itertools::Itertools;

use super::column::Column;
use crate::{
	HasIdent, Index, SchemaTable, TableColumn, TableForeignKey, TableIndex, TableItem,
	attribute::AttributeList,
	db_name_impls, def_name_impls,
	diagnostics::Report,
	index::{Check, PrimaryKey, UniqueConstraint},
	mixin::Mixin,
	names::{
		ColumnIdent, DbColumn, DbForeignKey, DbNativeType, DefName, ForeignKeyKind, MixinIdent,
		TableDefName, TableIdent, TableKind, TypeIdent,
	},
	scalar::PropagatedScalarData,
	uid::{OwnUid, RenameExt, RenameMap, Uid, next_uid},
	w,
};

#[derive(Debug)]
pub struct Table {
	uid: OwnUid,
	name: TableDefName,
	pub docs: Vec<String>,
	pub attrlist: AttributeList,
	pub columns: Vec<Column>,
	pub annotations: Vec<TableAnnotation>,
	pub foreign_keys: Vec<ForeignKey>,
	pub mixins: Vec<MixinIdent>,
}
def_name_impls!(Table, TableKind);
impl Table {
	pub fn new(
		docs: Vec<String>,
		attrlist: AttributeList,
		name: TableDefName,
		columns: Vec<Column>,
		annotations: Vec<TableAnnotation>,
		foreign_keys: Vec<ForeignKey>,
		mixins: Vec<MixinIdent>,
	) -> Self {
		Self {
			uid: next_uid(),
			name,
			docs,
			attrlist,
			columns,
			annotations,
			foreign_keys,
			mixins,
		}
	}
	pub fn primary_key(&self) -> Option<&PrimaryKey> {
		self.annotations
			.iter()
			.filter_map(TableAnnotation::as_primary_key)
			.at_most_one()
			.expect("at most one pk")
	}
	pub fn is_external(&self) -> bool {
		self.annotations
			.iter()
			.any(|a| matches!(a, TableAnnotation::External))
	}
	pub(crate) fn assimilate_mixin(&mut self, mixin: &Mixin) {
		let Mixin {
			docs,
			attrlist,
			columns,
			annotations,
			foreign_keys,
			mixins,
			..
		} = mixin;
		self.docs.extend_from_slice(docs);
		self.attrlist.0.extend_from_slice(&attrlist.0);
		self.columns
			.extend(columns.iter().map(|c| c.clone_for_mixin()));
		self.annotations
			.extend(annotations.iter().map(|a| a.clone_for_mixin()));
		self.foreign_keys
			.extend(foreign_keys.iter().map(|fk| fk.clone_for_mixin()));
		self.mixins.extend(mixins.clone());
	}
}
#[derive(Debug)]
pub enum TableAnnotation {
	Check(Check),
	Unique(UniqueConstraint),
	PrimaryKey(PrimaryKey),
	Index(Index),
	External,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum OnDelete {
	SetNull,
	SetDefault,
	Restrict,
	Noop,
	Cascade,
}
impl OnDelete {
	pub fn sql(&self) -> Option<&'static str> {
		Some(match self {
			OnDelete::SetNull => "SET NULL",
			OnDelete::SetDefault => "SET DEFAULT",
			OnDelete::Restrict => "RESTRICT",
			OnDelete::Noop => return None,
			OnDelete::Cascade => "CASCADE",
		})
	}
}

#[derive(Debug)]
pub struct ForeignKey {
	uid: OwnUid,
	name: Option<DbForeignKey>,
	pub source_fields: Option<Vec<ColumnIdent>>,
	pub target: TableIdent,
	pub target_fields: Option<Vec<ColumnIdent>>,
	pub on_delete: OnDelete,
}
db_name_impls!(ForeignKey, ForeignKeyKind);
impl ForeignKey {
	pub fn new(
		name: Option<DbForeignKey>,
		source_fields: Option<Vec<ColumnIdent>>,
		target: TableIdent,
		target_fields: Option<Vec<ColumnIdent>>,
		on_delete: OnDelete,
	) -> Self {
		Self {
			uid: next_uid(),
			name,
			source_fields,
			target,
			target_fields,
			on_delete,
		}
	}

	pub fn clone_for_mixin(&self) -> Self {
		Self::new(
			self.name.clone(),
			self.source_fields.clone(),
			self.target.clone(),
			self.target_fields.clone(),
			self.on_delete,
		)
	}
}

impl TableAnnotation {
	pub fn as_index(&self) -> Option<&Index> {
		match self {
			TableAnnotation::Index(i) => Some(i),
			_ => None,
		}
	}

	pub fn as_primary_key(&self) -> Option<&PrimaryKey> {
		if let Self::PrimaryKey(v) = self {
			Some(v)
		} else {
			None
		}
	}

	pub fn as_check(&self) -> Option<&Check> {
		if let Self::Check(c) = self {
			Some(c)
		} else {
			None
		}
	}

	pub fn as_unique_constraint(&self) -> Option<&UniqueConstraint> {
		if let Self::Unique(u) = self {
			Some(u)
		} else {
			None
		}
	}

	pub fn clone_for_mixin(&self) -> Self {
		match self {
			TableAnnotation::Check(c) => Self::Check(c.clone_for_propagate()),
			TableAnnotation::Unique(u) => Self::Unique(u.clone_for_propagate()),
			TableAnnotation::PrimaryKey(p) => Self::PrimaryKey(p.clone_for_propagate()),
			TableAnnotation::Index(i) => Self::Index(i.clone_for_propagate()),
			TableAnnotation::External => Self::External,
		}
	}
}

impl Table {
	pub fn process(&mut self) {
		for column in self.columns.iter_mut() {
			let propagated = column.propagate_annotations();
			self.annotations.extend(propagated);
			if let Some(fk) = column.propagate_foreign_key() {
				self.foreign_keys.push(fk);
			}
		}
	}
	pub(crate) fn propagate_scalar_data(
		&mut self,
		scalar: TypeIdent,
		propagated: &PropagatedScalarData,
	) {
		for col in self.columns.iter_mut() {
			col.propagate_scalar_data(scalar, propagated)
		}
	}

	pub fn db_name(&self, column: &ColumnIdent, rn: &RenameMap) -> DbColumn {
		for ele in self.columns.iter() {
			if &ele.id() == column {
				return ele.db(rn);
			}
		}
		let table = self.db(rn);
		unreachable!("unknown field: {table:?}.{column:?}");
	}
	pub fn db_names(
		&self,
		columns: impl IntoIterator<Item = ColumnIdent>,
		rn: &RenameMap,
	) -> Vec<DbColumn> {
		columns.into_iter().map(|c| self.db_name(&c, rn)).collect()
	}
	pub fn format_index_name(
		&self,
		columns: impl Iterator<Item = ColumnIdent>,
		rn: &RenameMap,
	) -> String {
		let mut out = String::new();
		for (i, column) in columns.enumerate() {
			if i != 0 {
				w!(out, "_");
			}
			let db_name = self.db_name(&column, rn);
			w!(out, "{}", db_name.raw());
		}
		assert!(!out.is_empty(), "passed no columns");
		out
	}
	pub fn print_column_list(
		&self,
		out: &mut String,
		columns: impl Iterator<Item = ColumnIdent>,
		rn: &RenameMap,
	) {
		for (i, column) in columns.enumerate() {
			if i != 0 {
				w!(out, ", ");
			}
			let db_name = self.db_name(&column, rn);
			w!(out, "{}", db_name.raw());
		}
	}
}
impl<'a> SchemaTable<'a> {
	fn item<I>(&self, value: &'a I) -> TableItem<'a, I> {
		TableItem::unchecked_new(*self, value)
	}
	pub fn schema_column(&'a self, column: ColumnIdent) -> TableColumn<'a> {
		self.columns()
			.find(|c| c.id() == column)
			.unwrap_or_else(|| panic!("column not found: {column:?}"))
	}
	pub fn columns(&self) -> impl Iterator<Item = TableColumn<'_>> {
		self.columns.iter().map(|i| self.item(i))
	}
	pub fn indexes(&self) -> impl Iterator<Item = TableIndex<'_>> {
		self.annotations
			.iter()
			.filter_map(TableAnnotation::as_index)
			.map(|i| self.item(i))
	}
	pub fn pk(&self) -> Option<&PrimaryKey> {
		self.annotations
			.iter()
			.filter_map(TableAnnotation::as_primary_key)
			.at_most_one()
			.expect("pk is not merged")
	}
	pub fn foreign_keys(&self) -> impl Iterator<Item = TableForeignKey<'_>> {
		self.foreign_keys.iter().map(|i| self.item(i))
	}
	pub fn cardinality(&self, columns: impl IntoIterator<Item = ColumnIdent>) -> Cardinality {
		fn is_unique_by_index(
			indexed: impl IntoIterator<Item = ColumnIdent>,
			columns: &BTreeSet<ColumnIdent>,
		) -> bool {
			let indexed = indexed.into_iter().collect::<BTreeSet<_>>();
			if indexed.difference(columns).next().is_none() {
				// Column list has full intersection with the unique index, thus
				// the request itself is unique
				//
				// I.e we have UNIQUE INDEX (a, b, c)
				// and the request is (a, b, c, d)
				//
				// Subset (a, b, c) is unique, thus d is irrelevant.
				return true;
			}
			false
		}
		let columns = columns.into_iter().collect::<BTreeSet<_>>();
		for ele in self.annotations.iter() {
			match ele {
				TableAnnotation::Index(i) => {
					if i.unique && is_unique_by_index(i.field_idents(), &columns) {
						return Cardinality::One;
					}
				}
				TableAnnotation::PrimaryKey(pk) => {
					if is_unique_by_index(pk.columns.iter().copied(), &columns) {
						return Cardinality::One;
					}
				}
				TableAnnotation::Unique(u) => {
					if is_unique_by_index(u.columns.iter().copied(), &columns) {
						return Cardinality::One;
					}
				}
				TableAnnotation::Check { .. } => {}
				TableAnnotation::External => {}
			}
		}
		// FIXME: Wrong assumption? When target is one, it doesn't mean the source is one too
		// for fk in self.foreign_keys() {
		// 	if !is_unique_by_index(fk.source_columns(), &columns) {
		// 		// This fk can not constraint our query
		// 		continue;
		// 	}
		// 	let target = fk.target_table();
		// 	let target_columns = fk.target_columns();
		// 	if target.cardinality(target_columns) == Cardinality::One {
		// 		return Cardinality::One;
		// 	}
		// }
		Cardinality::Many
	}
}

#[derive(PartialEq, Eq)]
pub enum Cardinality {
	One,
	Many,
}

impl<'s> TableForeignKey<'s> {
	pub fn source_columns(&self) -> Vec<ColumnIdent> {
		if let Some(source) = &self.source_fields {
			return source.clone();
		}
		if let Some(target) = &self.target_fields {
			return target.clone();
		};
		panic!("no fields defined")
	}
	pub fn target_columns(&self) -> Vec<ColumnIdent> {
		if let Some(target) = &self.target_fields {
			return target.clone();
		};
		if let Some(source) = &self.source_fields {
			return source.clone();
		}
		panic!("no fields defined")
	}
	pub fn target_table(&self) -> SchemaTable<'s> {
		let table = &self.target;
		self.table
			.schema
			.schema_table(table)
			.expect("target table is not defined")
	}
	pub fn source_db_columns(&self, rn: &RenameMap) -> Vec<DbColumn> {
		self.source_columns()
			.into_iter()
			.map(|f| self.table.db_name(&f, rn))
			.collect()
	}
	pub fn target_db_columns(&self, rn: &RenameMap) -> Vec<DbColumn> {
		let target_table = self.target_table();
		self.target_columns()
			.into_iter()
			.map(|f| target_table.db_name(&f, rn))
			.collect()
	}
	pub fn db_types(&self, rn: &RenameMap, report: &mut Report) -> Vec<DbNativeType> {
		let db_types: Vec<_> = self
			.source_columns()
			.into_iter()
			.map(|f| self.table.schema_column(f).db_type(rn, report))
			.collect();
		// Sanity
		let target_table = self.target_table();
		let target_db_types: Vec<_> = self
			.target_columns()
			.into_iter()
			.map(|f| target_table.schema_column(f).db_type(rn, report))
			.collect();
		assert_eq!(db_types, target_db_types);
		db_types
	}
	pub fn cardinality(&self) -> (Cardinality, Cardinality) {
		(
			self.table.cardinality(self.source_columns()),
			self.target_table().cardinality(self.target_columns()),
		)
	}
}
