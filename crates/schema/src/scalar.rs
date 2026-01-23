use std::mem;

use itertools::{Either, Itertools};

use super::sql::Sql;
use crate::{
	HasIdent, SchemaEnum, SchemaScalar,
	attribute::AttributeList,
	changelist::IsCompatible,
	column::ColumnAnnotation,
	composite::FieldAnnotation,
	def_name_impls, derive_is_isomorph_by_id_name,
	diagnostics::Report,
	index::{Check, Index, PrimaryKey, UniqueConstraint},
	names::{
		DbEnumItem, DbNativeType, EnumItemDefName, EnumItemKind, TypeDefName, TypeIdent, TypeKind,
	},
	root::Schema,
	uid::{OwnUid, RenameExt, RenameMap, next_uid},
};

#[derive(Debug)]
pub struct EnumItem {
	uid: OwnUid,
	pub docs: Vec<String>,
	name: EnumItemDefName,
}
impl EnumItem {
	pub fn new(docs: Vec<String>, name: EnumItemDefName) -> Self {
		Self {
			uid: next_uid(),
			docs,
			name,
		}
	}
}
def_name_impls!(EnumItem, EnumItemKind);
derive_is_isomorph_by_id_name!(EnumItem);

/// FIXME: Rename to EnumItem, rename old EnumItem to Variant
#[derive(Debug, Clone, Copy)]
pub struct EnumItemHandle<'a> {
	enum_: SchemaEnum<'a>,
	item: &'a EnumItem,
}
def_name_impls!(EnumItemHandle<'_>, EnumItemKind);
derive_is_isomorph_by_id_name!(EnumItemHandle<'_>);

impl std::ops::Deref for EnumItemHandle<'_> {
	type Target = EnumItem;

	fn deref(&self) -> &Self::Target {
		self.item
	}
}

impl IsCompatible for EnumItemHandle<'_> {
	fn is_compatible(
		&self,
		_new: &Self,
		_rn: &RenameMap,
		_a: &mut Report,
		_b: &mut Report,
	) -> bool {
		true
	}
}

#[derive(Debug)]
pub struct Enum {
	uid: OwnUid,
	name: TypeDefName,
	pub docs: Vec<String>,
	pub attrlist: AttributeList,
	pub items: Vec<EnumItem>,
}
impl Enum {
	pub fn new(
		docs: Vec<String>,
		attrlist: AttributeList,
		name: TypeDefName,
		items: Vec<EnumItem>,
	) -> Self {
		Self {
			uid: next_uid(),
			name,
			docs,
			attrlist,
			items,
		}
	}
	pub fn db_items(&self, rn: &RenameMap) -> Vec<DbEnumItem> {
		self.items.iter().map(|i| i.db(rn)).collect()
	}
	pub fn db_type(&self, rn: &RenameMap) -> DbNativeType {
		DbNativeType::unchecked_from(self.db(rn))
	}
}
def_name_impls!(Enum, TypeKind);

impl SchemaEnum<'_> {
	pub fn items(&self) -> impl Iterator<Item = EnumItemHandle<'_>> {
		self.items
			.iter()
			.map(|item| EnumItemHandle { enum_: *self, item })
	}
}

#[derive(Debug, Default)]
pub(crate) struct PropagatedScalarData {
	pub annotations: Vec<ColumnAnnotation>,
	pub field_annotations: Vec<FieldAnnotation>,
}
impl PropagatedScalarData {
	/// Returns true if extended
	pub(crate) fn extend(&mut self, other: Self) {
		self.annotations.extend(other.annotations);
		self.field_annotations.extend(other.field_annotations);
	}
	pub(crate) fn is_empty(&self) -> bool {
		self.annotations.is_empty() && self.field_annotations.is_empty()
	}
}

#[derive(Debug, Clone)]
pub enum InlineSqlTypePart {
	Raw(String),
	TypeRef(TypeIdent),
}
#[derive(Debug, Clone)]
pub struct InlineSqlType(pub Vec<InlineSqlTypePart>);
impl InlineSqlType {
	fn expand(&self, rn: &RenameMap, schema: &Schema, report: &mut Report) -> DbNativeType {
		let mut out = String::new();
		for p in &self.0 {
			match p {
				InlineSqlTypePart::Raw(r) => out.push_str(r),
				InlineSqlTypePart::TypeRef(ident) => {
					out.push_str(schema.native_type(ident, rn, report).raw())
				}
			}
		}
		DbNativeType::new(&out)
	}
	fn dependencies(&self, schema: &Schema, out: &mut Vec<TypeIdent>) {
		for v in &self.0 {
			if let InlineSqlTypePart::TypeRef(t) = v {
				schema.schema_ty(*t).depend_on(out);
			}
		}
	}
}

/// Even though CREATE DOMAIN allows domains to be constrained as non-nulls, currently it is not possible to make
/// scalar non-null, you should create a constraint, and then make a column not null, because non-null domain
/// might work not as user would expect. I.e it allows value to be null in case of outer joins.
#[derive(Debug)]
pub struct Scalar {
	uid: OwnUid,
	name: TypeDefName,
	pub docs: Vec<String>,
	pub attrlist: AttributeList,
	native: InlineSqlType,
	pub annotations: Vec<ScalarAnnotation>,
	inlined: bool,
}
def_name_impls!(Scalar, TypeKind);
impl Scalar {
	pub fn new(
		docs: Vec<String>,
		attrlist: AttributeList,
		name: TypeDefName,
		native: InlineSqlType,
		annotations: Vec<ScalarAnnotation>,
	) -> Self {
		Self {
			uid: next_uid(),
			name,
			docs,
			attrlist,
			native,
			annotations,
			inlined: false,
		}
	}
	pub(crate) fn propagate(&mut self, inline: bool) -> PropagatedScalarData {
		let annotations = mem::take(&mut self.annotations);
		let field_annotations = annotations
			.iter()
			.flat_map(|a| a.propagate_to_field(inline))
			.collect_vec();
		let (annotations, retained) = annotations
			.into_iter()
			.partition_map(|a| a.propagate_to_column(inline));
		self.annotations = retained;
		if inline {
			self.inlined = true;
		}
		PropagatedScalarData {
			annotations,
			field_annotations,
		}
	}
	pub fn is_always_inline(&self) -> bool {
		self.annotations
			.iter()
			.any(|a| matches!(a, ScalarAnnotation::Inline))
	}
	pub fn is_external(&self) -> bool {
		self.annotations
			.iter()
			.any(|a| matches!(a, ScalarAnnotation::External))
	}
	pub fn inlined(&self) -> bool {
		self.inlined
	}
}

impl SchemaScalar<'_> {
	pub fn depend_on(&self, out: &mut Vec<TypeIdent>) {
		assert!(
			!self.is_always_inline() || self.inlined,
			"always-inline type was not inlined"
		);
		if self.inlined() {
			self.type_dependencies(out);
		} else {
			out.push(self.id());
		}
	}
	pub fn type_dependencies(&self, out: &mut Vec<TypeIdent>) {
		assert!(
			!self.is_always_inline() || self.inlined,
			"always-inline type was not inlined"
		);
		self.native.dependencies(self.schema, out);
	}
	pub fn inner_type(&self, rn: &RenameMap, report: &mut Report) -> DbNativeType {
		self.native.expand(rn, self.schema, report)
	}
	pub fn native(&self, rn: &RenameMap, report: &mut Report) -> DbNativeType {
		assert!(
			!self.is_always_inline() || self.inlined,
			"always-inline type was not inlined"
		);
		if self.inlined {
			assert!(
				!self
					.annotations
					.iter()
					.any(ScalarAnnotation::is_inline_target),
				"inlined scalars may not have inline target scalars"
			);
			self.native.expand(rn, self.schema, report)
		} else {
			DbNativeType::unchecked_from(self.db(rn))
		}
	}
}

#[derive(Debug)]
pub enum ScalarAnnotation {
	/// Moved to column if inlined.
	Default(Sql),
	/// Moved to column if inlined.
	Check(Check),
	/// Moved to column.
	PrimaryKey(PrimaryKey),
	/// Moved to column.
	Unique(UniqueConstraint),
	/// Moved to column.
	Index(Index),
	// Always convert to native types, even if database supports domain types.
	Inline,
	External,
}
impl ScalarAnnotation {
	pub fn as_check(&self) -> Option<&Check> {
		match self {
			Self::Check(c) => Some(c),
			_ => None,
		}
	}
	/// Should annotation be removed after inlining?
	fn is_inline_target(&self) -> bool {
		!matches!(self, Self::Inline | Self::External)
	}
	fn propagate_to_field(&self, inline: bool) -> Option<FieldAnnotation> {
		Some(match self {
			ScalarAnnotation::Check(c) if inline => FieldAnnotation::Check(c.clone_for_propagate()),
			_ => return None,
		})
	}
	fn propagate_to_column(self, inline: bool) -> Either<ColumnAnnotation, Self> {
		Either::Left(match self {
			ScalarAnnotation::PrimaryKey(p) => ColumnAnnotation::PrimaryKey(p),
			ScalarAnnotation::Unique(u) => ColumnAnnotation::Unique(u),
			ScalarAnnotation::Index(i) => ColumnAnnotation::Index(i),
			ScalarAnnotation::Default(d) if inline => ColumnAnnotation::Default(d),
			ScalarAnnotation::Check(c) if inline => ColumnAnnotation::Check(c),
			_ => return Either::Right(self),
		})
	}
}
