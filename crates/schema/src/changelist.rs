//! Given the old and new list of named items, classify differences between them as renames (see `crate::renamelist`),
//! creates, drops, and updates.
//!
//! If values are not compatible by the decision of `IsCompatible` trait, then they will not be updated, and instead
//! old version will be dropped, and the new version will be created.

use std::{collections::HashSet, fmt::Debug};

use itertools::Itertools;

use crate::{
	Diff,
	diagnostics::Report,
	renamelist::{RenameMoveaway, RenameOp, RenameTempAllocator, reorder_renames},
	uid::{RenameExt, RenameMap},
};

#[derive(Debug, PartialEq)]
pub struct ChangeList<T: RenameExt> {
	pub dropped: Vec<T>,
	pub renamed: Vec<RenameOp<T>>,
	pub moved_away: Vec<(T, RenameMoveaway)>,
	pub updated: Vec<Diff<T>>,
	pub created: Vec<T>,
}
impl<T: RenameExt> ChangeList<T> {
	fn new() -> Self {
		Self {
			renamed: vec![],
			created: vec![],
			updated: vec![],
			dropped: vec![],
			moved_away: vec![],
		}
	}
}

pub trait IsCompatible {
	fn is_compatible(
		&self,
		new: &Self,
		rn: &RenameMap,
		report_self: &mut Report,
		report_new: &mut Report,
	) -> bool;
}
impl<T> IsCompatible for &T
where
	T: IsCompatible,
{
	fn is_compatible(
		&self,
		new: &Self,
		rn: &RenameMap,
		report_self: &mut Report,
		report_new: &mut Report,
	) -> bool {
		(**self).is_compatible(*new, rn, report_self, report_new)
	}
}

pub trait IsIsomorph {
	fn is_isomorph(
		&self,
		other: &Self,
		rn: &RenameMap,
		report_self: &mut Report,
		report_other: &mut Report,
	) -> bool;
}
impl<T> IsIsomorph for &T
where
	T: IsIsomorph,
{
	fn is_isomorph(
		&self,
		new: &Self,
		rn: &RenameMap,
		report_self: &mut Report,
		report_other: &mut Report,
	) -> bool {
		(**self).is_isomorph(*new, rn, report_self, report_other)
	}
}
#[macro_export]
macro_rules! derive_is_isomorph_by_id_name {
	($t:ty) => {
		impl $crate::IsIsomorph for $t {
			fn is_isomorph(
				&self,
				other: &Self,
				_rn: &$crate::RenameMap,
				_report_self: &mut $crate::diagnostics::Report,
				_report_other: &mut $crate::diagnostics::Report,
			) -> bool {
				use $crate::HasIdent;
				HasIdent::id(self).name() == HasIdent::id(other).name()
			}
		}
	};
}

pub fn mk_change_list<T: RenameExt + Clone + Copy + Debug, V: IsCompatible + IsIsomorph>(
	rn: &RenameMap,
	old: &[T],
	new: &[T],
	mapper: impl Fn(T) -> V,
	report_old: &mut Report,
	report_new: &mut Report,
) -> ChangeList<T> {
	let mut out = <ChangeList<T>>::new();

	let mut old_listed = HashSet::new();
	let mut new_listed = HashSet::new();

	// Final list of occupied items
	let occupancy = new.iter().map(|n| n.db(rn)).collect::<HashSet<_>>();

	for (oid, old) in old.iter().cloned().enumerate() {
		let mut new_by_exact = new.iter().cloned().enumerate().filter(|(i, f)| {
			!new_listed.contains(i)
				&& mapper(*f).is_isomorph(&mapper(old), rn, report_new, report_old)
				&& f.db(rn) == old.db(rn)
		});
		if let Some((nid, new)) = new_by_exact.next() {
			{
				let other = new_by_exact.next();
				assert!(
					other.is_none(),
					"second exact match shouldn't be possible: {nid} {new:?} {other:?}"
				);
			}
			old_listed.insert(oid);
			new_listed.insert(nid);
			out.updated.push(Diff { old, new });
			continue;
		}
	}
	for (oid, old) in old.iter().cloned().enumerate() {
		if old_listed.contains(&oid) {
			continue;
		}
		let mut new_by_code = new.iter().cloned().enumerate().filter(|(i, f)| {
			mapper(*f).is_isomorph(&mapper(old), rn, report_new, report_old)
				&& !new_listed.contains(i)
		});
		if let Some((nid, new)) = new_by_code.next() {
			assert!(new_by_code.next().is_none());
			old_listed.insert(oid);
			new_listed.insert(nid);
			out.updated.push(Diff { old, new });
			continue;
		}
	}

	let mut out_dropped = Vec::new();

	let mut allocator = RenameTempAllocator::default();
	for (oid, old) in old.iter().cloned().enumerate() {
		if old_listed.contains(&oid) {
			continue;
		}
		let mut new_by_db = new
			.iter()
			.cloned()
			.enumerate()
			.filter(|(i, f)| f.db(rn) == old.db(rn) && !new_listed.contains(i));
		if let Some((nid, new)) = new_by_db.next() {
			assert!(new_by_db.next().is_none());
			old_listed.insert(oid);
			new_listed.insert(nid);
			out.updated.push(Diff { old, new });
			continue;
		}
		let tmp = if occupancy.contains(&old.db(rn)) {
			Some(allocator.next_moveaway())
		} else {
			None
		};
		out_dropped.push((old, tmp));
	}

	for recreated in out
		.updated
		.iter()
		.filter(|diff| {
			!mapper(diff.old).is_compatible(&mapper(diff.new), rn, report_old, report_new)
		})
		.collect::<Vec<_>>()
	{
		out.created.push(recreated.new);
		out_dropped.push((recreated.old, Some(allocator.next_moveaway())));
	}
	out.updated.retain(|diff| {
		mapper(diff.old).is_compatible(&mapper(diff.new), rn, report_old, report_new)
	});

	for (_, new) in new
		.iter()
		.cloned()
		.enumerate()
		.filter(|(i, _)| !new_listed.contains(i))
	{
		out.created.push(new);
	}

	let mut to_rename = vec![];
	for updated in out.updated.iter() {
		to_rename.push((updated.old, updated.new.db(rn), updated.old));
	}
	let mut moveaways = vec![];
	for old_dropped in out_dropped.iter() {
		if let Some(tmp) = old_dropped.1 {
			moveaways.push((old_dropped.0, tmp));
		} else if new.iter().any(|n| n.db(rn) == old_dropped.0.db(rn)) {
			moveaways.push((old_dropped.0, allocator.next_moveaway()));
		}
	}
	out.renamed = reorder_renames(rn, to_rename, moveaways.clone(), &mut allocator);
	out.moved_away = moveaways;
	out.dropped = out_dropped.into_iter().map(|(v, _)| v).collect_vec();

	out
}

// Always disabled, any() == false
#[cfg(all(test, any()))]
mod tests {
	use crate::{
		HasDefaultDbName, HasIdent,
		changelist::{ChangeList, IsCompatible},
		ids::{DbIdent, Ident, in_allocator},
		mk_change_list,
		names::{DefName, TypeKind},
		renamelist::{RenameOp, RenameTempAllocator},
		span::{SimpleSpan, register_source},
		uid::{HasUid, OwnUid, RenameMap, Uid, next_uid},
	};
	#[test]
	fn changelist_conflict() {
		tracing_subscriber::fmt().init();
		in_allocator(|| {
			#[derive(Debug, PartialEq)]
			struct P(OwnUid, DefName<TypeKind>);
			derive_is_isomorph_by_id_name!(P);
			impl IsCompatible for P {
				fn is_compatible(&self, _new: &Self, _rn: &RenameMap) -> bool {
					true
				}
			}
			impl HasDefaultDbName for P {
				type Kind = TypeKind;
				fn default_db(&self) -> Option<DbIdent<Self::Kind>> {
					self.1.default_db()
				}
			}
			impl HasIdent for P {
				type Kind = TypeKind;
				fn id(&self) -> Ident<Self::Kind> {
					self.1.id()
				}
			}
			impl HasUid for P {
				fn uid(&self) -> Uid {
					self.0.uid()
				}
			}
			fn p(a: &'static str, b: &'static str) -> P {
				let s = a.to_string();
				let s = register_source(s);
				P(
					next_uid(),
					DefName::alloc((SimpleSpan::new(s, 0, a.len() as u32), a, Some(b))),
				)
			}
			fn i(n: &'static str) -> DbIdent<TypeKind> {
				DbIdent::new(n)
			}
			macro_rules! diff {
				($a:expr, $b:expr) => {
					crate::Diff { old: $a, new: $b }
				};
			}
			let mut ren = RenameTempAllocator::default();
			let ren1 = ren.next_temp();

			assert_eq!(
				mk_change_list(
					&RenameMap::default(),
					&[p("A", "D"), p("C", "B")],
					&[p("C", "D"), p("A", "B")]
				),
				ChangeList {
					renamed: vec![
						RenameOp::Store(p("A", "D"), ren1),
						RenameOp::Rename(p("C", "B"), i("D")),
						RenameOp::Restore(ren1, i("B"))
					],
					updated: vec![
						diff!(p("A", "D"), p("A", "B")),
						diff!(p("C", "B"), p("C", "D"))
					],

					created: vec![],
					dropped: vec![],
					moved_away: vec![],
				}
			);
			assert_eq!(
				mk_change_list(
					&RenameMap::default(),
					&[p("A", "B")],
					&[p("A", "C"), p("D", "B")]
				),
				ChangeList {
					renamed: vec![RenameOp::Rename(p("A", "B"), i("C"))],
					updated: vec![diff!(p("A", "B"), p("A", "C"))],
					created: vec![p("D", "B")],
					moved_away: vec![],

					dropped: vec![],
				}
			);
			assert_eq!(
				mk_change_list(
					&RenameMap::default(),
					&[p("D", "B"), p("A", "C")],
					&[p("A", "B")]
				),
				ChangeList {
					renamed: vec![
						RenameOp::Store(p("D", "B"), ren1),
						RenameOp::Rename(p("A", "C"), i("B"))
					],
					updated: vec![diff!(p("A", "C"), p("A", "B")),],
					dropped: vec![p("D", "B")],
					moved_away: vec![(p("D", "B"), ren1)],

					created: vec![],
				}
			);
		});
	}
}
