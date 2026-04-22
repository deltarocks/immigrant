use std::{cell::Cell, collections::HashMap};

use crate::{HasDefaultDbName, ids::DbIdent};

#[derive(PartialEq, Eq, Hash, Debug, Clone, Copy)]
pub struct Uid(usize);

/// OwnUid - Uid attached to attribute/something else
/// It doesn't make sense to clone attribute Uid, because if we do so, then
/// changing name of attribute on table column also changes attribute name in scalar/other copies of this attribute
/// Because of that, cloning of OwnUid should also clone entry of RenameMap.
#[derive(PartialEq, Eq, Hash, Debug)]
#[repr(transparent)]
pub struct OwnUid(Uid);
impl HasUid for OwnUid {
	fn uid(&self) -> Uid {
		self.0
	}
}

thread_local! {
	static LAST_UID: Cell<usize> = Default::default();
}

pub(crate) fn next_uid() -> OwnUid {
	OwnUid(LAST_UID.with(|u| {
		let v = u.get();
		u.set(v.checked_add(1).expect("overflow"));
		Uid(v)
	}))
}

#[derive(Default, Clone)]
pub struct RenameMap(HashMap<Uid, String>);
impl RenameMap {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn merge(&mut self, other: RenameMap) {
		for (k, v) in other.0 {
			assert!(
				self.0.insert(k, v).is_none(),
				"cannot merge RenameMap with conflicting entries"
			)
		}
	}
}

pub trait HasUid {
	fn uid(&self) -> Uid;
}
impl<T> HasUid for &T
where
	T: HasUid,
{
	fn uid(&self) -> Uid {
		(*self).uid()
	}
}

pub trait RenameExt {
	type Kind;
	fn db(&self, rn: &RenameMap) -> DbIdent<Self::Kind> {
		self.try_db(rn).expect("name was not assigned")
	}
	fn try_db(&self, rn: &RenameMap) -> Option<DbIdent<Self::Kind>>;
	fn set_db(&self, rn: &mut RenameMap, name: DbIdent<Self::Kind>);
	fn db_assigned(&self, rn: &RenameMap) -> bool;
}
impl<T: HasDefaultDbName + HasUid> RenameExt for T {
	type Kind = T::Kind;
	fn try_db(&self, rn: &RenameMap) -> Option<DbIdent<Self::Kind>> {
		if let Some(v) = rn.0.get(&self.uid()) {
			Some(DbIdent::new(v.as_str()))
		} else {
			self.default_db()
		}
	}
	fn set_db(&self, rn: &mut RenameMap, name: DbIdent<Self::Kind>) {
		rn.0.insert(self.uid(), name.raw().to_string());
	}
	fn db_assigned(&self, rn: &RenameMap) -> bool {
		rn.0.contains_key(&self.uid()) || self.default_db().is_some()
	}
}
