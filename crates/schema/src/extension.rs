use crate::{
	annotation::AnnotationList,
	def_name_impls,
	names::{ExtensionDefName, ExtensionKind},
	uid::{OwnUid, next_uid},
};

#[derive(Debug)]
pub enum ExtensionAttribute {
	External,
	Version(String),
	Cascade,
}

#[derive(Debug)]
pub struct Extension {
	uid: OwnUid,
	name: ExtensionDefName,
	pub docs: Vec<String>,
	pub annotations: AnnotationList,
	pub attributes: Vec<ExtensionAttribute>,
}
def_name_impls!(Extension, ExtensionKind);
impl Extension {
	pub fn new(
		docs: Vec<String>,
		annotations: AnnotationList,
		name: ExtensionDefName,
		attributes: Vec<ExtensionAttribute>,
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
			.any(|a| matches!(a, ExtensionAttribute::External))
	}
	pub fn cascade(&self) -> bool {
		self.attributes
			.iter()
			.any(|a| matches!(a, ExtensionAttribute::Cascade))
	}
	pub fn version(&self) -> Option<&str> {
		self.attributes.iter().find_map(|a| match a {
			ExtensionAttribute::Version(v) => Some(v.as_str()),
			_ => None,
		})
	}
}
