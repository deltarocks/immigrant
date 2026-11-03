use std::{
	cell::{Cell, RefCell},
	cmp::Ordering,
	collections::HashMap,
	fmt::{self, Debug},
	hash::{Hash, Hasher},
	marker::PhantomData,
};

use derivative::Derivative;

use crate::{names::UnknownKind, span::SimpleSpan};

#[derive(Default)]
struct Interner {
	ids: HashMap<String, u32>,
	values: Vec<String>,
}
impl Interner {
	fn intern(&mut self, value: &str) -> u32 {
		if let Some(id) = self.ids.get(value) {
			return *id;
		}
		let id = u32::try_from(self.values.len()).expect("out of ids");
		self.values.push(value.to_owned());
		self.ids.insert(value.to_owned(), id);
		id
	}
	fn get(&self, id: u32) -> &str {
		&self.values[id as usize]
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub(crate) struct NameId(u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId(u32);
impl ModuleId {
	pub fn new(path: &str) -> Self {
		ALLOCATOR.with(|a| ModuleId(a.1.modules.borrow_mut().intern(path)))
	}
	pub fn path(&self) -> String {
		ALLOCATOR.with(|a| a.1.modules.borrow().get(self.0).to_owned())
	}
}
impl Debug for ModuleId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{:?}", self.path())
	}
}

#[derive(Default)]
pub struct CodeIdentAllocator {
	names: RefCell<Interner>,
	modules: RefCell<Interner>,

	#[cfg(feature = "strict")]
	generation: Cell<u32>,
}
impl CodeIdentAllocator {
	#[cfg(feature = "strict")]
	pub fn next_generation(&self) {
		self.generation.set(self.generation.get() + 1);
	}
	fn name(&self, id: NameId) -> String {
		self.names.borrow().get(id.0).to_owned()
	}
	fn ident<K: Kind>(
		&self,
		span: SimpleSpan,
		name: &str,
		module: Option<Option<ModuleId>>,
	) -> Ident<K> {
		let name = NameId(self.names.borrow_mut().intern(name));
		Ident {
			#[cfg(feature = "strict")]
			generation: self.generation.get(),
			#[cfg(feature = "strict")]
			kind: K::id(),
			state: match module {
				Some(module) => IdentState::Resolved(module, name),
				None => IdentState::Unresolved(name),
			},
			span,
			_marker: PhantomData,
		}
	}
}

pub(crate) fn intern_name(name: &str) -> NameId {
	ALLOCATOR.with(|a| NameId(a.1.names.borrow_mut().intern(name)))
}
pub(crate) fn name_of(id: NameId) -> String {
	ALLOCATOR.with(|a| a.1.name(id))
}

pub trait Kind {
	fn id() -> u8;
}

thread_local! {
	static ALLOCATOR: (Cell<bool>, CodeIdentAllocator) = (Cell::new(false), CodeIdentAllocator::default());
}
pub fn in_allocator<T>(f: impl FnOnce() -> T) -> T {
	ALLOCATOR.with(|a| {
		assert!(!a.0.get(), "already in allocator");
		a.0.set(true);
	});
	let v = f();
	ALLOCATOR.with(|a| {
		assert!(a.0.get(), "should be in allocator");
		a.0.set(false);
		#[cfg(feature = "strict")]
		a.1.next_generation();
	});
	v
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum IdentState {
	Unresolved(NameId),
	Resolved(Option<ModuleId>, NameId),
}

pub struct Ident<K> {
	#[cfg(feature = "strict")]
	generation: u32,
	#[cfg(feature = "strict")]
	kind: u8,
	state: IdentState,
	span: SimpleSpan,
	_marker: PhantomData<fn() -> K>,
}
impl<K> Ident<K> {
	pub(crate) fn name_id(&self) -> NameId {
		match self.state {
			IdentState::Unresolved(name) | IdentState::Resolved(_, name) => name,
		}
	}
	pub fn name(&self) -> String {
		name_of(self.name_id())
	}
	pub fn span(&self) -> SimpleSpan {
		self.span
	}
	pub fn is_resolved(&self) -> bool {
		matches!(self.state, IdentState::Resolved(..))
	}
	pub fn module(&self) -> Option<ModuleId> {
		self.key().0
	}
	pub fn qualified_name(&self) -> (Option<String>, String) {
		(self.module().map(|m| m.path()), self.name())
	}
	pub(crate) fn set_resolved(&mut self, module: Option<ModuleId>, name: NameId) {
		self.state = IdentState::Resolved(module, name);
	}
	pub(crate) fn erase(self) -> Ident<()> {
		Ident {
			#[cfg(feature = "strict")]
			generation: self.generation,
			#[cfg(feature = "strict")]
			kind: self.kind,
			state: self.state,
			span: self.span,
			_marker: PhantomData,
		}
	}
	fn key(&self) -> (Option<ModuleId>, NameId) {
		match self.state {
			IdentState::Resolved(module, name) => (module, name),
			IdentState::Unresolved(name) => panic!(
				"identifier `{}` is used before name resolution",
				name_of(name)
			),
		}
	}
	#[cfg(feature = "strict")]
	fn assert_comparable(&self, other: &Self) {
		assert!(
			self.generation == other.generation && self.kind == other.kind,
			"comparing idents of different generations"
		);
	}
	#[cfg(not(feature = "strict"))]
	fn assert_comparable(&self, _other: &Self) {}
}
impl<K: Kind> Ident<K> {
	fn alloc(span: SimpleSpan, name: &str, module: Option<Option<ModuleId>>) -> Self {
		ALLOCATOR.with(|a| {
			assert!(a.0.get(), "should be in allocator");
			a.1.ident(span, name, module)
		})
	}
	pub fn reference((span, name): (SimpleSpan, &str)) -> Self {
		Self::alloc(span, name, None)
	}
	pub fn member((span, name): (SimpleSpan, &str)) -> Self {
		Self::alloc(span, name, Some(None))
	}
	pub fn definition((span, name): (SimpleSpan, &str), module: ModuleId) -> Self {
		Self::alloc(span, name, Some(Some(module)))
	}
	pub fn unchecked_cast<U: Kind>(v: Ident<U>) -> Self {
		assert_eq!(
			K::id(),
			U::id(),
			"types should be explicitly marked as compatible"
		);
		Ident {
			#[cfg(feature = "strict")]
			generation: v.generation,
			#[cfg(feature = "strict")]
			kind: v.kind,
			state: v.state,
			span: v.span,
			_marker: PhantomData,
		}
	}
	pub fn to_unknown(self) -> Ident<UnknownKind> {
		// Not using unchecked_cast to skip compatibility check
		Ident {
			#[cfg(feature = "strict")]
			generation: self.generation,
			#[cfg(feature = "strict")]
			kind: UnknownKind::id(),
			state: self.state,
			span: self.span,
			_marker: PhantomData,
		}
	}
}
impl<K> Clone for Ident<K> {
	fn clone(&self) -> Self {
		*self
	}
}
impl<K> Copy for Ident<K> {}
impl<K> PartialEq for Ident<K> {
	fn eq(&self, other: &Ident<K>) -> bool {
		self.assert_comparable(other);
		self.key() == other.key()
	}
}
impl<K> Eq for Ident<K> {}
impl<K> Ord for Ident<K> {
	fn cmp(&self, other: &Ident<K>) -> Ordering {
		self.assert_comparable(other);
		self.key().cmp(&other.key())
	}
}
impl<K> Hash for Ident<K> {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.key().hash(state);
	}
}
impl<K> PartialOrd for Ident<K> {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}
impl<K> Debug for Ident<K> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.name())
	}
}

#[derive(Derivative)]
#[derivative(Default(bound = ""), Ord(bound = ""))]
pub struct DbIdent<K> {
	id: String,
	_marker: PhantomData<K>,
}
impl<K> PartialOrd for DbIdent<K> {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}
impl<K> DbIdent<K> {
	pub fn new(v: &str) -> Self {
		assert_ne!(v, "");
		Self {
			id: v.to_owned(),
			_marker: PhantomData,
		}
	}
	pub fn raw(&self) -> &str {
		&self.id
	}
}
impl<K> PartialEq for DbIdent<K> {
	fn eq(&self, other: &Self) -> bool {
		self.id == other.id
	}
}
impl<K> Eq for DbIdent<K> {}
impl<K> Hash for DbIdent<K> {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.id.hash(state);
	}
}
// impl<K> Display for DbIdent<K> {
// 	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
// 		self.assert_not_guard();
// 		write!(f, "{}", self.id)
// 	}
// }
impl<K> Debug for DbIdent<K> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{:?}", self.id)
	}
}
impl<K> Clone for DbIdent<K> {
	fn clone(&self) -> Self {
		Self {
			id: self.id.clone(),
			_marker: PhantomData,
		}
	}
}

impl<T> DbIdent<T> {
	pub fn unchecked_from<U>(t: DbIdent<U>) -> Self {
		DbIdent {
			id: t.id,
			_marker: PhantomData,
		}
	}
}
