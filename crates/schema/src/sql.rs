use crate::{
	HasIdent, SchemaItem, SchemaType,
	diagnostics::Report,
	ids::{DbIdent, Ident},
	names::{ColumnIdent, DbProcedure, FieldIdent, FieldKind, TypeIdent, TypeKind},
	uid::{RenameExt, RenameMap},
};

#[derive(Debug, Clone)]
pub enum SqlUnOp {
	Plus,
	Minus,
	Not,
}
impl SqlUnOp {
	pub fn format(&self) -> &'static str {
		match self {
			SqlUnOp::Plus => "+",
			SqlUnOp::Minus => "-",
			SqlUnOp::Not => "NOT ",
		}
	}
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlOp {
	Lt,
	Gt,
	Le,
	Ge,
	Eq,
	Ne,
	SEq,
	SNe,

	And,
	Or,

	Plus,
	Minus,
	Div,
	Mul,
	Mod,

	Like,
}
impl SqlOp {
	pub fn format(&self) -> &'static str {
		match self {
			SqlOp::Lt => "<",
			SqlOp::Gt => ">",
			SqlOp::Le => "<=",
			SqlOp::Ge => ">=",
			SqlOp::Eq => "=",
			SqlOp::Ne => "<>",
			SqlOp::SEq => "IS NOT DISTINCT FROM",
			SqlOp::SNe => "IS DISTINCT FROM",
			SqlOp::And => "AND",
			SqlOp::Or => "OR",
			SqlOp::Plus => "+",
			SqlOp::Minus => "-",
			SqlOp::Div => "/",
			SqlOp::Mul => "*",
			SqlOp::Mod => "%",
			SqlOp::Like => "LIKE",
		}
	}
}
#[derive(Debug, Clone)]
pub enum Sql {
	Cast(Box<Sql>, TypeIdent),
	Call(DbProcedure, Vec<Sql>),
	String(String),
	Number(i128),
	Ident(ColumnIdent),
	UnOp(SqlUnOp, Box<Sql>),
	BinOp(Box<Sql>, SqlOp, Box<Sql>),
	GetField(Box<Sql>, FieldIdent),
	Parened(Box<Sql>),
	Tuple(Vec<Sql>),
	If(Box<Sql>, Box<Sql>, Box<Sql>),
	Boolean(bool),
	Placeholder,
	Null,
}
impl Sql {
	pub fn replace_placeholder(&mut self, to: Sql) {
		struct ReplacePlaceholder {
			to: Sql,
		}
		impl SqlVisitor for ReplacePlaceholder {
			fn handle_placeholder(&mut self, placeholder: &mut Sql) {
				*placeholder = self.to.clone()
			}
		}
		self.visit(&mut ReplacePlaceholder { to })
	}
	pub fn affected_columns(&self) -> Vec<ColumnIdent> {
		struct ColumnCollector {
			columns: Vec<ColumnIdent>,
		}
		impl SqlVisitor for ColumnCollector {
			fn handle_column(&mut self, column: &mut ColumnIdent) {
				if self.columns.contains(column) {
					return;
				}
				self.columns.push(*column)
			}
			fn handle_placeholder(&mut self, _placeholder: &mut Sql) {
				panic!("placeholder was not normalized")
			}
		}
		let mut collector = ColumnCollector { columns: vec![] };
		let mut copy = self.clone();
		copy.visit(&mut collector);
		collector.columns
	}
	fn visit(&mut self, v: &mut impl SqlVisitor) {
		v.handle(self);
		match self {
			Sql::Cast(s, t) => {
				s.visit(v);
				v.handle_type(t);
			}
			Sql::Call(_p, args) => {
				for arg in args {
					arg.visit(v);
				}
			}
			Sql::String(_s) => {}
			Sql::Number(_n) => {}
			Sql::Ident(i) => v.handle_column(i),
			Sql::UnOp(_o, s) => s.visit(v),
			Sql::BinOp(a, _o, b) => {
				a.visit(v);
				b.visit(v);
			}
			Sql::Parened(s) => s.visit(v),
			Sql::Placeholder => v.handle_placeholder(self),
			Sql::Boolean(_) => {}
			Sql::GetField(s, _) => s.visit(v),
			Sql::Null => {}
			Sql::Tuple(els) => {
				for ele in els.iter_mut() {
					ele.visit(v)
				}
			}
			Sql::If(a, b, c) => {
				a.visit(v);
				b.visit(v);
				c.visit(v);
			}
		}
	}
	pub fn all(s: impl IntoIterator<Item = Self>) -> Self {
		let mut s = s.into_iter();
		let mut v = s.next().unwrap_or(Self::Boolean(true));
		for i in s {
			v = Sql::BinOp(Box::new(v), SqlOp::And, Box::new(i))
		}
		v
	}
	pub fn any(s: impl IntoIterator<Item = Self>) -> Self {
		let mut s = s.into_iter();
		let mut v = s.next().unwrap_or(Self::Boolean(false));
		for i in s {
			v = Sql::BinOp(Box::new(v), SqlOp::Or, Box::new(i))
		}
		v
	}
	pub fn field_name(
		&self,
		context: &SchemaItem<'_>,
		field: Ident<FieldKind>,
		rn: &RenameMap,
		report: &mut Report,
	) -> Option<DbIdent<FieldKind>> {
		let this = self.type_ident_of_expr(context, report)?;
		let ty = context.schema().schema_ty(this);
		Some(match ty {
			SchemaType::Enum(e) => {
				report
					.error("enum variants can't be observed yet")
					.annotate("happened here", field.span())
					.annotate("enum defined here", e.id().span());
				return None;
			}
			SchemaType::Scalar(s) => {
				report
					.error("invalid field access")
					.annotate("happened here", field.span())
					.annotate("scalars can't have fields", s.id().span());
				return None;
			}
			SchemaType::Composite(c) => c.field(field).db(rn),
		})
	}
	pub fn context_ident_name<'s>(
		context: &'s SchemaItem<'s>,
		ident: ColumnIdent,
		rn: &RenameMap,
		report: &mut Report,
	) -> Option<DbIdent<FieldKind>> {
		Some(match context {
			SchemaItem::Table(t) => {
				let column = t.schema_column(ident);
				DbIdent::unchecked_from(column.db(rn))
			}
			SchemaItem::Enum(e) => {
				report
					.error("enum variants can't be observed yet")
					.annotate("happened here", ident.span())
					.annotate("enum defined here", e.id().span());
				return None;
			}
			SchemaItem::Scalar(s) => {
				report
					.error("invalid field access")
					.annotate("happened here", ident.span())
					.annotate("scalars can't have fields", s.id().span());
				return None;
			}
			SchemaItem::Composite(c) => {
				let field = c.field(Ident::unchecked_cast(ident));
				field.db(rn)
			}
			SchemaItem::View(v) => {
				report
					.error("invalid field access")
					.annotate("happened here", ident.span())
					.annotate("views are opaque", v.id().span());
				return None;
			}
		})
	}
	/// If self == Sql::Ident(name), convert name to native
	pub fn ident_name<'s>(
		&self,
		context: &'s SchemaItem<'s>,
		rn: &RenameMap,
		report: &mut Report,
	) -> Option<DbIdent<FieldKind>> {
		let Sql::Ident(f) = self else {
			panic!("not ident");
		};
		Self::context_ident_name(context, *f, rn, report)
	}
	fn type_ident_of_expr<'s>(
		&self,
		context: &'s SchemaItem<'s>,
		report: &mut Report,
	) -> Option<Ident<TypeKind>> {
		Some(match self {
			Sql::Cast(_, t) => *t,
			Sql::UnOp(_, _)
			| Sql::BinOp(_, _, _)
			| Sql::Call(_, _)
			| Sql::String(_)
			| Sql::Number(_)
			| Sql::Boolean(_)
			| Sql::Null
			| Sql::Tuple(_)
			| Sql::If(_, _, _) => {
				panic!("cannot determine call return type")
			}
			Sql::Ident(f) => match context {
				SchemaItem::Table(t) => {
					let column = t.schema_column(*f);
					column.ty
				}
				SchemaItem::Enum(e) => {
					report
						.error("invalid value reference")
						.annotate("enums can't contain fields", f.span())
						.annotate("enum defined here", e.id().span());
					return None;
				}
				SchemaItem::Scalar(s) => {
					report
						.error("invalid value reference")
						.annotate("scalars can't contain fields", f.span())
						.annotate("scalar defined here", s.id().span());
					return None;
				}
				SchemaItem::Composite(c) => {
					let field = c.field(Ident::unchecked_cast(*f));
					field.ty
				}
				SchemaItem::View(v) => {
					report
						.error("invalid value reference")
						.annotate("views can't contain fields", f.span())
						.annotate("scalar defined here", v.id().span());
					return None;
				}
			},
			Sql::GetField(f, t) => {
				let ty_id = f.type_ident_of_expr(context, report)?;
				let ty = context.schema().schema_ty(ty_id);
				Sql::Ident(Ident::unchecked_cast(*t))
					.type_ident_of_expr(&ty.as_schema_item(), report)?
			}
			Sql::Parened(v) => v.type_ident_of_expr(context, report)?,
			Sql::Placeholder => match context {
				SchemaItem::Table(_) | SchemaItem::View(_) => {
					panic!("can't refer to table fields using this notation")
				}
				SchemaItem::Enum(e) => e.id(),
				SchemaItem::Scalar(s) => s.id(),
				SchemaItem::Composite(c) => c.id(),
			},
		})
	}
}
#[allow(unused_variables)]
pub trait SqlVisitor {
	fn handle(&mut self, sql: &mut Sql) {}
	fn handle_type(&mut self, ty: &mut TypeIdent) {}
	fn handle_column(&mut self, column: &mut ColumnIdent) {}
	fn handle_placeholder(&mut self, placeholder: &mut Sql) {}
}
