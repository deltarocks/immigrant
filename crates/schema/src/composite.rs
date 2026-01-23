use std::{mem, ops::Deref};

use itertools::{Either, Itertools};

use crate::{
	HasIdent, IsCompatible, SchemaComposite, SchemaType,
	attribute::AttributeList,
	column::ColumnAnnotation,
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
pub enum FieldAnnotation {
	Check(Check),
}
impl FieldAnnotation {
	fn propagate_to_composite(self, field: FieldIdent) -> Either<CompositeAnnotation, Self> {
		Either::Left(match self {
			FieldAnnotation::Check(c) => {
				CompositeAnnotation::Check(c.propagate_to_composite(field))
			}
			#[allow(unreachable_patterns)]
			_ => return Either::Right(self),
		})
	}
	fn clone_for_propagate(&self) -> Self {
		match self {
			FieldAnnotation::Check(c) => Self::Check(c.clone_for_propagate()),
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
	pub annotations: Vec<FieldAnnotation>,
}
def_name_impls!(Field, FieldKind);
derive_is_isomorph_by_id_name!(Field);

impl Field {
	pub fn new(
		docs: Vec<String>,
		name: CompositeItemDefName,
		nullable: bool,
		ty: TypeIdent,
		annotations: Vec<FieldAnnotation>,
	) -> Self {
		Self {
			uid: next_uid(),
			docs,
			name,
			nullable,
			ty,
			annotations,
		}
	}
	pub(crate) fn propagate_scalar_data(
		&mut self,
		scalar: TypeIdent,
		propagated: &PropagatedScalarData,
	) {
		if self.ty == scalar {
			self.annotations.extend(
				propagated
					.field_annotations
					.iter()
					.map(|a| a.clone_for_propagate()),
			);
		}
	}
	pub fn propagate_annotations(&mut self) -> Vec<CompositeAnnotation> {
		let (annotations, retained) = mem::take(&mut self.annotations)
			.into_iter()
			.partition_map(|a| a.propagate_to_composite(self.id()));
		self.annotations = retained;
		annotations
	}
}

#[derive(Debug)]
pub enum CompositeAnnotation {
	Check(Check),
}
impl CompositeAnnotation {
	fn propagate_to_field(&self) -> Option<FieldAnnotation> {
		Some(match self {
			CompositeAnnotation::Check(c) => FieldAnnotation::Check(c.clone_for_propagate()),
		})
	}
	fn propagate_to_column(self) -> Either<ColumnAnnotation, Self> {
		Either::Left(match self {
			CompositeAnnotation::Check(c) => ColumnAnnotation::Check(c),
		})
	}
}

#[derive(Debug)]
pub struct Composite {
	uid: OwnUid,
	name: TypeDefName,
	pub docs: Vec<String>,
	pub attrlist: AttributeList,
	pub fields: Vec<Field>,
	pub annotations: Vec<CompositeAnnotation>,
}
def_name_impls!(Composite, TypeKind);

impl Composite {
	pub fn new(
		docs: Vec<String>,
		attrlist: AttributeList,
		name: TypeDefName,
		fields: Vec<Field>,
		mut annotations: Vec<CompositeAnnotation>,
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
			annotations.push(CompositeAnnotation::Check(Check::new(
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
			attrlist,
			fields,
			annotations,
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
			let propagated = column.propagate_annotations();
			self.annotations.extend(propagated);
		}
	}

	pub(crate) fn propagate(&mut self) -> PropagatedScalarData {
		let annotations = mem::take(&mut self.annotations);
		let field_annotations = annotations
			.iter()
			.flat_map(CompositeAnnotation::propagate_to_field)
			.collect_vec();
		let (annotations, retained) = annotations
			.into_iter()
			.partition_map(|a| a.propagate_to_column());
		self.annotations = retained;
		PropagatedScalarData {
			annotations,
			field_annotations,
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
