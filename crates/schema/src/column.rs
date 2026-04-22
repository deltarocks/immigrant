use std::mem;

use itertools::{Either, Itertools};

use super::{
	index::Index,
	sql::Sql,
	table::{ForeignKey, TableAttribute},
};
use crate::{
	HasIdent, SchemaType, TableColumn,
	annotation::AnnotationList,
	changelist::IsCompatible,
	def_name_impls, derive_is_isomorph_by_id_name,
	diagnostics::Report,
	index::{Check, PrimaryKey, UniqueConstraint},
	names::{ColumnDefName, ColumnIdent, ColumnKind, DbNativeType, DefName, TypeIdent},
	scalar::PropagatedScalarData,
	uid::{OwnUid, RenameMap, next_uid},
};

#[derive(Debug)]
pub enum ColumnAttribute {
	/// Moved to table.
	Check(Check),
	/// Moved to table.
	Unique(UniqueConstraint),
	/// Moved to table.
	PrimaryKey(PrimaryKey),
	/// Moved to table.
	Index(Index),
	/// Column default attribute.
	Default(Sql),
	/// If column is created, then this attribute is used to prefill it, treat it as one-time, much more powerful
	/// DEFAULT value.
	/// It can be implemented as an UPDATE statement, or with SET TYPE ... USING, but not as one-time DEFAULT, because
	/// it can reference other columns.
	///
	/// If column is updated, then it is just passed to USING expression, allowing to refernce older column value,
	/// just be careful with that. Initially this expression was only usable for new column creation, but
	/// after further consideration I decided there is no harm of allowing it for upgrades.
	/// Yes, this is a semi-imperative action, however it is pretty isolated and subtle to make it actually work.
	///
	/// After all, it doesn't allow access to old schema version fields directly (except for the current field),
	/// thus not breaking isolation of a standalone schema definition.
	InitializeAs(Sql),
}
impl ColumnAttribute {
	fn as_default(&self) -> Option<&Sql> {
		match self {
			Self::Default(s) => Some(s),
			_ => None,
		}
	}
	fn as_initialize_as(&self) -> Option<&Sql> {
		match self {
			Self::InitializeAs(s) => Some(s),
			_ => None,
		}
	}
	fn propagate_to_table(self, column: ColumnIdent) -> Either<TableAttribute, Self> {
		Either::Left(match self {
			ColumnAttribute::Check(c) => TableAttribute::Check(c.propagate_to_table(column)),
			ColumnAttribute::Unique(u) => TableAttribute::Unique(u.propagate_to_table(column)),
			ColumnAttribute::PrimaryKey(p) => {
				TableAttribute::PrimaryKey(p.propagate_to_table(column))
			}
			ColumnAttribute::Index(i) => TableAttribute::Index(i.propagate_to_table(column)),
			_ => return Either::Right(self),
		})
	}
	fn clone_for_mixin(&self) -> Self {
		self.clone_for_propagate()
	}
	fn clone_for_propagate(&self) -> Self {
		match self {
			ColumnAttribute::Check(c) => Self::Check(c.clone_for_propagate()),
			ColumnAttribute::Unique(u) => Self::Unique(u.clone_for_propagate()),
			ColumnAttribute::PrimaryKey(p) => Self::PrimaryKey(p.clone_for_propagate()),
			ColumnAttribute::Index(i) => Self::Index(i.clone_for_propagate()),
			ColumnAttribute::Default(d) => Self::Default(d.clone()),
			ColumnAttribute::InitializeAs(i) => Self::InitializeAs(i.clone()),
		}
	}
}

#[derive(Debug)]
pub struct Column {
	uid: OwnUid,
	name: ColumnDefName,
	pub docs: Vec<String>,
	pub annotations: AnnotationList,
	pub nullable: bool,
	pub ty: TypeIdent,
	pub attributes: Vec<ColumnAttribute>,
	pub foreign_key: Option<PartialForeignKey>,
}
def_name_impls!(Column, ColumnKind);
derive_is_isomorph_by_id_name!(Column);
impl Column {
	pub fn new(
		name: ColumnDefName,
		docs: Vec<String>,
		annotations: AnnotationList,
		nullable: bool,
		ty: TypeIdent,
		attributes: Vec<ColumnAttribute>,
		foreign_key: Option<PartialForeignKey>,
	) -> Self {
		Self {
			uid: next_uid(),
			annotations,
			name,
			docs,
			nullable,
			ty,
			attributes,
			foreign_key,
		}
	}
}
impl IsCompatible for Column {
	fn is_compatible(&self, _new: &Self, _rn: &RenameMap, _a: &mut Report, _b: &mut Report) -> bool {
		true
	}
}

#[derive(Debug)]
pub struct PartialForeignKey {
	pub fk: ForeignKey,
}
impl PartialForeignKey {
	fn clone_for_mixin(&self) -> Self {
		Self {
			fk: self.fk.clone_for_mixin(),
		}
	}
}

impl Column {
	pub(crate) fn propagate_scalar_data(
		&mut self,
		scalar: TypeIdent,
		propagated: &PropagatedScalarData,
	) {
		if self.ty == scalar {
			self.attributes.extend(
				propagated
					.attributes
					.iter()
					.map(|v| v.clone_for_propagate()),
			);
		}
	}
	pub fn propagate_attributes(&mut self) -> Vec<TableAttribute> {
		let (attributes, retained) = mem::take(&mut self.attributes)
			.into_iter()
			.partition_map(|a| a.propagate_to_table(self.id()));
		self.attributes = retained;
		attributes
	}
	pub fn propagate_foreign_key(&mut self) -> Option<ForeignKey> {
		let mut fk = self.foreign_key.take()?;
		fk.fk.source_fields = Some(vec![self.id()]);
		Some(fk.fk)
	}

	pub fn clone_for_mixin(&self) -> Column {
		Column::new(
			DefName::unchecked_new(
				self.name.code,
				// Makes little sense to support RenameMap here, columns renamed in Mixins are not propagated.
				self.name.db.clone(),
			),
			self.docs.clone(),
			self.annotations.clone(),
			self.nullable,
			self.ty,
			self.attributes
				.iter()
				.map(|a| a.clone_for_mixin())
				.collect(),
			self.foreign_key.as_ref().map(|fk| fk.clone_for_mixin()),
		)
	}
}

impl<'a> TableColumn<'a> {
	pub fn db_type(&self, rn: &RenameMap, report: &mut Report) -> DbNativeType {
		self.table.schema.native_type(&self.ty, rn, report)
	}
	pub fn ty(&'a self) -> SchemaType<'a> {
		self.table.schema.schema_ty(self.ty)
	}
	/// Only returns column default, if the underlying type has default value -
	/// it needs to be handled manually.
	/// If you only want to check if default exists - use has_default.
	pub fn default(&self) -> Option<&Sql> {
		self.attributes
			.iter()
			.filter_map(|v| v.as_default())
			.at_most_one()
			.unwrap()
	}
	pub fn initialize_as(&self) -> Option<&Sql> {
		self.attributes
			.iter()
			.filter_map(|v| v.as_initialize_as())
			.at_most_one()
			.unwrap()
	}
	pub fn is_pk_part(&self) -> bool {
		let Some(pk) = self.table.pk() else {
			return false;
		};
		pk.columns.contains(&self.id())
	}
	pub fn has_default(&self) -> bool {
		self.default().is_some() || self.ty().has_default()
	}
	pub fn is_pk_full(&self) -> bool {
		let Some(pk) = self.table.pk() else {
			return false;
		};
		pk.columns == [self.id()]
	}
}
