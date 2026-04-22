use crate::annotation::AnnotationList;
use crate::column::Column;
use crate::id_impls;
use crate::names::{MixinIdent, MixinKind};
use crate::table::{ForeignKey, TableAttribute};
use crate::uid::{OwnUid, next_uid};

#[derive(Debug)]
pub struct Mixin {
	uid: OwnUid,
	name: MixinIdent,
	pub docs: Vec<String>,
	pub annotations: AnnotationList,
	pub columns: Vec<Column>,
	pub attributes: Vec<TableAttribute>,
	pub foreign_keys: Vec<ForeignKey>,
	pub mixins: Vec<MixinIdent>,
}
id_impls!(Mixin, MixinKind);
impl Mixin {
	pub fn new(
		docs: Vec<String>,
		annotations: AnnotationList,
		name: MixinIdent,
		columns: Vec<Column>,
		attributes: Vec<TableAttribute>,
		foreign_keys: Vec<ForeignKey>,
		mixins: Vec<MixinIdent>,
	) -> Self {
		Self {
			uid: next_uid(),
			name,
			docs,
			annotations,
			columns,
			attributes,
			foreign_keys,
			mixins,
		}
	}
}
