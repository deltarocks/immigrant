use crate::attribute::AttributeList;
use crate::column::Column;
use crate::id_impls;
use crate::names::{MixinIdent, MixinKind};
use crate::table::{ForeignKey, TableAnnotation};
use crate::uid::{OwnUid, next_uid};

#[derive(Debug)]
pub struct Mixin {
	uid: OwnUid,
	name: MixinIdent,
	pub docs: Vec<String>,
	pub attrlist: AttributeList,
	pub columns: Vec<Column>,
	pub annotations: Vec<TableAnnotation>,
	pub foreign_keys: Vec<ForeignKey>,
	pub mixins: Vec<MixinIdent>,
}
id_impls!(Mixin, MixinKind);
impl Mixin {
	pub fn new(
		docs: Vec<String>,
		attrlist: AttributeList,
		name: MixinIdent,
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
}
