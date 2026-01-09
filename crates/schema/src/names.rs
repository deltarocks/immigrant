use std::fmt::{self, Debug};

use crate::{
	ids::{DbIdent, Ident, Kind},
	span::SimpleSpan,
	HasDefaultDbName, HasIdent,
};

macro_rules! def_kind {
    ($($name:ident($v:expr_2021)),+ $(,)?) => {
        $(
            pub enum $name {}
            impl Kind for $name {
                fn id() -> u8 {
                    $v
                }
            }
        )+
    };
}

def_kind!(
	ColumnKind(2),
	FieldKind(2),
	ProcedureKind(3),
	EnumItemKind(4),
	IndexKind(5),
	ConstraintKind(6),
	ForeignKeyKind(7),
	NativeTypeKind(8),
	ItemKind(9),
	TableKind(9),
	MixinKind(9),
	TypeKind(9),
	ViewKind(9),
	TriggerKind(9),
	UnknownKind(20),
);

pub struct DefName<K> {
	pub code: Ident<K>,
	pub db: Option<DbIdent<K>>,
}
impl<K: Kind> DefName<K> {
	pub fn unchecked_new(code: Ident<K>, db: Option<DbIdent<K>>) -> Self {
		Self { code, db }
	}
	pub fn alloc((span, code, db): (SimpleSpan, &str, Option<&str>)) -> Self {
		Self {
			code: Ident::alloc((span, code)),
			db: db.map(DbIdent::new),
		}
	}
	pub fn unchecked_cast<U: Kind>(v: DefName<U>) -> Self {
		Self {
			code: Ident::unchecked_cast(v.code),
			db: v.db.map(DbIdent::unchecked_from),
		}
	}
}
impl<K> HasIdent for DefName<K> {
	type Kind = K;
	fn id(&self) -> Ident<Self::Kind> {
		self.code
	}
}
impl<K> HasDefaultDbName for DefName<K> {
	type Kind = K;
	fn default_db(&self) -> Option<DbIdent<Self::Kind>> {
		self.db.clone()
	}
}
impl<K> Debug for DefName<K> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{:?} \"{:?}\"", self.code, self.db)
	}
}
impl<K> PartialEq<Ident<K>> for DefName<K> {
	fn eq(&self, other: &Ident<K>) -> bool {
		&self.code == other
	}
}
impl<K> PartialEq for DefName<K> {
	fn eq(&self, other: &Self) -> bool {
		self.code == other.code && self.db == other.db
	}
}

impl<K> Clone for DefName<K> {
	fn clone(&self) -> Self {
		Self {
			code: self.code,
			db: self.db.clone(),
		}
	}
}

pub type TableDefName = DefName<TableKind>;
pub type ViewDefName = DefName<ViewKind>;
pub type TriggerDefName = DefName<TriggerKind>;
pub type ColumnDefName = DefName<ColumnKind>;
pub type TypeDefName = DefName<TypeKind>;
pub type EnumItemDefName = DefName<EnumItemKind>;
pub type CompositeItemDefName = DefName<FieldKind>;

pub type TableIdent = Ident<TableKind>;
pub type ViewIdent = Ident<ViewKind>;
pub type TriggerIdent = Ident<TriggerKind>;
pub type ColumnIdent = Ident<ColumnKind>;
pub type TypeIdent = Ident<TypeKind>;
pub type FieldIdent = Ident<FieldKind>;
pub type MixinIdent = Ident<MixinKind>;

pub type DbTable = DbIdent<TableKind>;
pub type DbView = DbIdent<ViewKind>;
pub type DbTrigger = DbIdent<TriggerKind>;
pub type DbColumn = DbIdent<ColumnKind>;
pub type DbType = DbIdent<TypeKind>;
pub type DbProcedure = DbIdent<ProcedureKind>;
pub type DbIndex = DbIdent<IndexKind>;
pub type DbConstraint = DbIdent<ConstraintKind>;
pub type DbForeignKey = DbIdent<ForeignKeyKind>;
pub type DbEnumItem = DbIdent<EnumItemKind>;
pub type DbNativeType = DbIdent<NativeTypeKind>;
pub type DbItem = DbIdent<ItemKind>;
