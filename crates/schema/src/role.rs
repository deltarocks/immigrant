use crate::{
	annotation::AnnotationList,
	db_name_impls, def_name_impls,
	names::{ColumnIdent, DbPolicy, PolicyKind, RoleDefName, RoleIdent, RoleKind},
	sql::Sql,
	uid::{OwnUid, Uid, next_uid},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
	Select,
	Insert,
	Update,
	Delete,
}
impl Permission {
	pub fn sql(&self) -> &'static str {
		match self {
			Permission::Select => "SELECT",
			Permission::Insert => "INSERT",
			Permission::Update => "UPDATE",
			Permission::Delete => "DELETE",
		}
	}
}

#[derive(Debug)]
pub enum RoleAttribute {
	External,
	Default(Vec<Permission>),
}

#[derive(Debug)]
pub struct Role {
	uid: OwnUid,
	name: RoleDefName,
	pub docs: Vec<String>,
	pub annotations: AnnotationList,
	pub attributes: Vec<RoleAttribute>,
}
def_name_impls!(Role, RoleKind);
impl Role {
	pub fn new(
		docs: Vec<String>,
		annotations: AnnotationList,
		name: RoleDefName,
		attributes: Vec<RoleAttribute>,
	) -> Self {
		Self {
			uid: next_uid(),
			name,
			docs,
			annotations,
			attributes,
		}
	}
	pub fn is_external(&self) -> bool {
		self.attributes
			.iter()
			.any(|a| matches!(a, RoleAttribute::External))
	}
	pub fn default_permissions(&self) -> Vec<Permission> {
		let mut out = Vec::new();
		for attr in &self.attributes {
			if let RoleAttribute::Default(perms) = attr {
				out.extend(perms.iter().copied());
			}
		}
		out
	}
}

#[derive(Debug, Clone)]
pub struct RoleGrant {
	pub role: RoleIdent,
	pub permissions: Vec<Permission>,
}

#[derive(Debug)]
pub struct Policy {
	uid: OwnUid,
	name: Option<DbPolicy>,
	pub role: RoleIdent,
	pub check: Sql,
}
db_name_impls!(Policy, PolicyKind);
impl Policy {
	pub fn new(name: Option<DbPolicy>, role: RoleIdent, check: Sql) -> Self {
		Self {
			uid: next_uid(),
			name,
			role,
			check,
		}
	}
	pub fn propagate_to_table(mut self, column: ColumnIdent) -> Self {
		self.check.replace_placeholder(Sql::Ident(column));
		self
	}
	pub fn clone_for_propagate(&self) -> Self {
		Self {
			uid: next_uid(),
			name: self.name.clone(),
			role: self.role,
			check: self.check.clone(),
		}
	}
}
