use std::{mem, ops::Deref};

use itertools::{Either, Itertools};

use crate::{
	HasIdent, IsCompatible, SchemaComposite, SchemaType,
	annotation::AnnotationList,
	column::ColumnAttribute,
	def_name_impls, derive_is_isomorph_by_id_name,
	diagnostics::Report,
	ids::DbIdent,
	index::Check,
	names::{
		CompositeItemDefName, DbNativeType, FieldIdent, FieldKind, TypeDefName, TypeIdent, TypeKind,
	},
	scalar::PropagatedScalarData,
	sql::Sql,
	uid::{OwnUid, RenameExt, RenameMap, next_uid},
};

#[derive(Debug)]
pub enum FieldAttribute {
	Check(Check),
}
impl FieldAttribute {
	fn propagate_to_composite(self, field: FieldIdent) -> Either<CompositeAttribute, Self> {
		Either::Left(match self {
			FieldAttribute::Check(c) => {
				CompositeAttribute::Check(c.propagate_to_composite(field))
			}
			#[allow(unreachable_patterns)]
			_ => return Either::Right(self),
		})
	}
	fn clone_for_propagate(&self) -> Self {
		match self {
			FieldAttribute::Check(c) => Self::Check(c.clone_for_propagate()),
		}
	}
}

#[derive(Debug)]
pub struct Field {
	uid: OwnUid,
	name: CompositeItemDefName,
	pub docs: Vec<String>,
	pub nullable: bool,
	pub ty: TypeIdent,
	pub attributes: Vec<FieldAttribute>,
}
def_name_impls!(Field, FieldKind);
derive_is_isomorph_by_id_name!(Field);

impl Field {
	pub fn new(
		docs: Vec<String>,
		name: CompositeItemDefName,
		nullable: bool,
		ty: TypeIdent,
		attributes: Vec<FieldAttribute>,
	) -> Self {
		Self {
			uid: next_uid(),
			docs,
			name,
			nullable,
			ty,
			attributes,
		}
	}
	pub(crate) fn propagate_scalar_data(
		&mut self,
		scalar: TypeIdent,
		propagated: &PropagatedScalarData,
	) {
		if self.ty == scalar {
			self.attributes.extend(
				propagated
					.field_attributes
					.iter()
					.map(|a| a.clone_for_propagate()),
			);
		}
	}
	pub fn propagate_attributes(&mut self) -> Vec<CompositeAttribute> {
		let (attributes, retained) = mem::take(&mut self.attributes)
			.into_iter()
			.partition_map(|a| a.propagate_to_composite(self.id()));
		self.attributes = retained;
		attributes
	}
}

#[derive(Debug)]
pub enum CompositeAttribute {
	Check(Check),
}
impl CompositeAttribute {
	fn propagate_to_field(&self) -> Option<FieldAttribute> {
		Some(match self {
			CompositeAttribute::Check(c) => FieldAttribute::Check(c.clone_for_propagate()),
		})
	}
	fn propagate_to_column(self) -> Either<ColumnAttribute, Self> {
		Either::Left(match self {
			CompositeAttribute::Check(c) => ColumnAttribute::Check(c),
		})
	}
}

#[derive(Debug)]
pub struct Composite {
	uid: OwnUid,
	name: TypeDefName,
	pub docs: Vec<String>,
	pub annotations: AnnotationList,
	pub fields: Vec<Field>,
	pub attributes: Vec<CompositeAttribute>,
}
def_name_impls!(Composite, TypeKind);

impl Composite {
	pub fn new(
		docs: Vec<String>,
		annotations: AnnotationList,
		name: TypeDefName,
		fields: Vec<Field>,
		mut attributes: Vec<CompositeAttribute>,
	) -> Self {
		let mut checks = vec![];
		for field in &fields {
			if field.nullable {
				continue;
			}
			checks.push(Sql::BinOp(
				Box::new(Sql::GetField(Box::new(Sql::Placeholder), field.id())),
				crate::sql::SqlOp::SNe,
				Box::new(Sql::Null),
			))
		}
		if !checks.is_empty() {
			attributes.push(CompositeAttribute::Check(Check::new(
				Some(DbIdent::new("composite_nullability_check")),
				Sql::any([
					Sql::BinOp(
						Box::new(Sql::Placeholder),
						crate::sql::SqlOp::SEq,
						Box::new(Sql::Null),
					),
					Sql::all(checks),
				]),
			)));
		}
		Self {
			uid: next_uid(),
			name,
			docs,
			annotations,
			fields,
			attributes,
		}
	}
	pub fn db_type(&self, rn: &RenameMap) -> DbNativeType {
		DbNativeType::unchecked_from(self.db(rn))
	}
	pub(crate) fn propagate_scalar_data(
		&mut self,
		scalar: TypeIdent,
		propagated: &PropagatedScalarData,
	) {
		for col in self.fields.iter_mut() {
			col.propagate_scalar_data(scalar, propagated)
		}
	}
	pub fn process(&mut self) {
		for column in self.fields.iter_mut() {
			let propagated = column.propagate_attributes();
			self.attributes.extend(propagated);
		}
	}

	pub(crate) fn propagate(&mut self) -> PropagatedScalarData {
		let attributes = mem::take(&mut self.attributes);
		let field_attributes = attributes
			.iter()
			.flat_map(CompositeAttribute::propagate_to_field)
			.collect_vec();
		let (attributes, retained) = attributes
			.into_iter()
			.partition_map(|a| a.propagate_to_column());
		self.attributes = retained;
		PropagatedScalarData {
			attributes,
			field_attributes,
		}
	}
}

impl SchemaComposite<'_> {
	pub fn field(&self, field: FieldIdent) -> CompositeField<'_> {
		self.fields()
			.find(|c| c.id() == field)
			.expect("field not found")
	}
	pub fn fields(&self) -> impl Iterator<Item = CompositeField<'_>> {
		self.fields.iter().map(|field| CompositeField {
			composite: *self,
			field,
		})
	}
}

#[derive(Debug, Clone, Copy)]
pub struct CompositeField<'a> {
	composite: SchemaComposite<'a>,
	field: &'a Field,
}
def_name_impls!(CompositeField<'_>, FieldKind);
derive_is_isomorph_by_id_name!(CompositeField<'_>);

impl IsCompatible for CompositeField<'_> {
	fn is_compatible(
		&self,
		new: &Self,
		rn: &RenameMap,
		report_self: &mut Report,
		report_new: &mut Report,
	) -> bool {
		self.name.db == new.name.db && self.db_type(rn, report_self) == new.db_type(rn, report_new)
	}
}

impl Deref for CompositeField<'_> {
	type Target = Field;

	fn deref(&self) -> &Self::Target {
		self.field
	}
}

impl CompositeField<'_> {
	pub fn ty(&self) -> SchemaType<'_> {
		self.composite.schema.schema_ty(self.ty)
	}
	pub fn db_type(&self, rn: &RenameMap, report: &mut Report) -> DbNativeType {
		self.composite.schema.native_type(&self.ty, rn, report)
	}
}
