use super::sql::Sql;
use crate::{
	TableIndex, db_name_impls,
	diagnostics::Report,
	ids::DbIdent,
	names::{ColumnIdent, ConstraintKind, DbColumn, DbNativeType, FieldIdent, IndexKind},
	uid::{OwnUid, RenameMap, Uid, next_uid},
};

/// Can appear on columns, scalars, and tables.
///
/// Checks with the same name are merged using sql AND operator.
#[derive(Debug)]
pub struct Check {
	uid: OwnUid,
	name: Option<DbIdent<ConstraintKind>>,
	pub check: Sql,
}
db_name_impls!(Check, ConstraintKind);
impl Check {
	pub fn new(name: Option<DbIdent<ConstraintKind>>, check: Sql) -> Self {
		Self {
			uid: next_uid(),
			name,
			check,
		}
	}
	pub fn propagate_to_table(mut self, column: ColumnIdent) -> Self {
		self.check.replace_placeholder(Sql::Ident(column));
		self
	}
	pub fn propagate_to_composite(mut self, field: FieldIdent) -> Self {
		self.check
			.replace_placeholder(Sql::GetField(Box::new(Sql::Placeholder), field));
		self
	}
	pub fn clone_for_propagate(&self) -> Self {
		Check {
			uid: next_uid(),
			name: self.name.clone(),
			check: self.check.clone(),
		}
	}
}

/// Can appear on columns, scalars, and tables.
///
/// Constraints with the same name are merged, unifying columns.
/// When appears on a scalar - always inlined to column.
#[derive(Debug)]
pub struct UniqueConstraint {
	uid: OwnUid,
	name: Option<DbIdent<ConstraintKind>>,
	pub columns: Vec<ColumnIdent>,
}
db_name_impls!(UniqueConstraint, ConstraintKind);
impl UniqueConstraint {
	pub fn new(name: Option<DbIdent<ConstraintKind>>, columns: Vec<ColumnIdent>) -> Self {
		Self {
			uid: next_uid(),
			name,
			columns,
		}
	}
	pub fn propagate_to_table(mut self, column: ColumnIdent) -> Self {
		self.columns.insert(0, column);
		self
	}
	pub fn clone_for_propagate(&self) -> Self {
		Self {
			uid: next_uid(),
			name: self.name.clone(),
			columns: self.columns.clone(),
		}
	}
}

/// Can appear on columns, scalars, and tables.
///
/// Only tables can define name for primary key, in other cases it will raise a validation error.
/// Always merged, if there is a name conflict - raises a validation error.
/// When appears on a scalar - always inlined to column.
#[derive(Debug)]
pub struct PrimaryKey {
	uid: OwnUid,
	name: Option<DbIdent<ConstraintKind>>,
	pub columns: Vec<ColumnIdent>,
}
db_name_impls!(PrimaryKey, ConstraintKind);
impl PrimaryKey {
	pub fn new(name: Option<DbIdent<ConstraintKind>>, columns: Vec<ColumnIdent>) -> Self {
		Self {
			uid: next_uid(),
			name,
			columns,
		}
	}
	pub fn propagate_to_table(mut self, column: ColumnIdent) -> Self {
		self.columns.insert(0, column);
		self
	}
	pub fn clone_for_propagate(&self) -> Self {
		Self {
			uid: next_uid(),
			name: self.name.clone(),
			columns: self.columns.clone(),
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Using(pub String);
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpClass(pub String);
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct With(pub String);

/// Can appear on columns, scalars, and tables.
///
/// Indexed with the same name are merged, if one index is marked as unique, and other isn't - raises a validation error.
// TODO: Index kind? BTREE/etc
#[derive(Debug)]
pub struct Index {
	uid: OwnUid,
	name: Option<DbIdent<IndexKind>>,
	pub unique: bool,
	fields: Vec<(ColumnIdent, Option<OpClass>)>,
	pub using: Option<Using>,
	// FIXME: Should only exist when Index is defined on field, this opclass is applied to the field itself, not to the index
	pub default_opclass: Option<OpClass>,
	pub with: Option<With>,
}
db_name_impls!(Index, IndexKind);
impl Index {
	pub fn new(
		name: Option<DbIdent<IndexKind>>,
		unique: bool,
		fields: Vec<(ColumnIdent, Option<OpClass>)>,
		using: Option<Using>,
		default_opclass: Option<OpClass>,
		with: Option<With>,
	) -> Self {
		Self {
			uid: next_uid(),
			name,
			unique,
			fields: fields
				.into_iter()
				.map(|v| (v.0, v.1.or(default_opclass.clone())))
				.collect(),
			using,
			default_opclass,
			with,
		}
	}
	pub fn propagate_to_table(mut self, column: ColumnIdent) -> Self {
		self.fields
			.insert(0, (column, self.default_opclass.clone()));
		self
	}
	pub fn fields(&self) -> &[(ColumnIdent, Option<OpClass>)] {
		&self.fields
	}
	pub fn field_idents(&self) -> impl IntoIterator<Item = ColumnIdent> + '_ {
		self.fields().iter().map(|i| i.0)
	}
	pub fn clone_for_propagate(&self) -> Self {
		Self {
			uid: next_uid(),
			name: self.name.clone(),
			unique: self.unique,
			fields: self.fields.clone(),
			using: self.using.clone(),
			default_opclass: self.default_opclass.clone(),
			with: self.with.clone(),
		}
	}
}
impl TableIndex<'_> {
	pub fn db_columns<'i>(&'i self, rn: &'i RenameMap) -> impl Iterator<Item = DbColumn> + 'i {
		self.fields.iter().map(|f| self.table.db_name(&f.0, rn))
	}
	pub fn db_columns_opclass<'i>(
		&'i self,
		rn: &'i RenameMap,
	) -> impl Iterator<Item = (DbColumn, Option<OpClass>)> + 'i {
		self.fields
			.iter()
			.map(|f| (self.table.db_name(&f.0, rn), f.1.clone()))
	}
	pub fn db_types<'i>(
		&'i self,
		rn: &'i RenameMap,
		report: &'i mut Report,
	) -> impl Iterator<Item = DbNativeType> + 'i {
		self.fields
			.iter()
			.map(|f| self.table.schema_column(f.0).db_type(rn, report))
	}
}
