use crate::{
	attribute::AttributeList,
	def_name_impls,
	names::{ColumnIdent, TableIdent, ViewDefName, ViewKind},
	uid::{OwnUid, Uid, next_uid},
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
pub struct View {
	uid: OwnUid,
	name: ViewDefName,
	pub docs: Vec<String>,
	pub attrlist: AttributeList,
	pub materialized: bool,
	pub definition: Definition,
}
def_name_impls!(View, ViewKind);
impl View {
	pub fn new(
		docs: Vec<String>,
		attrlist: AttributeList,
		name: ViewDefName,
		materialized: bool,
		definition: Definition,
	) -> Self {
		Self {
			uid: next_uid(),
			name,
			docs,
			attrlist,
			materialized,
			definition,
		}
	}
}
