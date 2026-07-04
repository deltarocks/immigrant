use crate::{
	annotation::AnnotationList,
	def_name_impls,
	names::{ColumnIdent, TableIdent, ViewDefName, ViewKind},
	role::RoleGrant,
	uid::{OwnUid, next_uid},
};

#[derive(Debug)]
pub enum DefinitionPart {
	Raw(String),
	TableRef(TableIdent),
	ColumnRef(TableIdent, ColumnIdent),
}
#[derive(Debug)]
pub struct Definition(pub Vec<DefinitionPart>);

#[derive(Debug)]
pub enum ViewAttribute {
	SecurityDefiner,
	RoleGrant(RoleGrant),
}
impl ViewAttribute {
	pub fn as_role_grant(&self) -> Option<&RoleGrant> {
		if let Self::RoleGrant(g) = self {
			Some(g)
		} else {
			None
		}
	}
}

#[derive(Debug)]
pub struct View {
	uid: OwnUid,
	name: ViewDefName,
	pub docs: Vec<String>,
	pub annotations: AnnotationList,
	pub materialized: bool,
	pub attributes: Vec<ViewAttribute>,
	pub definition: Definition,
}
def_name_impls!(View, ViewKind);
impl View {
	pub fn new(
		docs: Vec<String>,
		annotations: AnnotationList,
		name: ViewDefName,
		materialized: bool,
		attributes: Vec<ViewAttribute>,
		definition: Definition,
	) -> Self {
		Self {
			uid: next_uid(),
			name,
			docs,
			annotations,
			materialized,
			attributes,
			definition,
		}
	}
	pub fn security_invoker(&self) -> bool {
		!self.materialized
			&& !self
				.attributes
				.iter()
				.any(|a| matches!(a, ViewAttribute::SecurityDefiner))
	}
	pub fn role_grants(&self) -> impl Iterator<Item = &RoleGrant> {
		self.attributes
			.iter()
			.filter_map(ViewAttribute::as_role_grant)
	}
}
