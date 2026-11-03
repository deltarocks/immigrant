use std::{
	collections::BTreeMap,
	result,
	sync::{self, atomic::AtomicUsize},
};

use peg::{error::ParseError, parser, str::LineCol};

use crate::{
	annotation::{Annotation, AnnotationField, AnnotationList, AnnotationValue},
	column::{Column, ColumnAttribute, PartialForeignKey},
	composite::{Composite, CompositeAttribute, Field, FieldAttribute},
	diagnostics::Report,
	extension::{Extension, ExtensionAttribute},
	ids::{DbIdent, ModuleId},
	index::{Check, Index, OpClass, PrimaryKey, UniqueConstraint, Using, With},
	mixin::Mixin,
	names::{
		ColumnIdent, DbProcedure, DefName, EnumItemDefName, ExtensionDefName, FieldIdent,
		MemberRename, MixinIdent, RoleDefName, RoleIdent, TableDefName, TableIdent, TypeDefName,
		TypeIdent, ViewDefName,
	},
	project::parse_project,
	role::{Permission, Policy, Role, RoleAttribute, RoleGrant},
	root::{Item, Schema, SchemaProcessOptions},
	scalar::{Enum, EnumItem, InlineSqlType, InlineSqlTypePart, Scalar, ScalarAttribute},
	source::ResolvedSpec,
	span::{SimpleSpan, SourceId},
	sql::{Sql, SqlOp, SqlUnOp},
	table::{ForeignKey, OnDelete, Table, TableAttribute},
	uid::RenameMap,
	view::{Definition, DefinitionPart, View, ViewAttribute},
};

fn h<T>(v: T) -> Box<T> {
	Box::new(v)
}

enum InlinePart<T> {
	Text(String),
	Template(T),
}

// V0: "INTEGER" syntax allowed in place of sql"INTEGER"
// V1: views were security_invoker = false by default, making underlying tables RLS ignored during SELECT.
pub const LATEST_SCHEMA_VERSION: u32 = 2;

pub const ROOT_MODULE: &str = "db.schema";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaVersion {
	Current,
	Stored(u32),
}

#[derive(Clone)]
pub(crate) struct AliasDecl {
	pub name: (SimpleSpan, String),
	pub target: (SimpleSpan, String),
}

#[derive(Clone)]
pub(crate) struct ImportName {
	pub name: (SimpleSpan, String),
	pub local: Option<(SimpleSpan, String)>,
}
impl ImportName {
	pub fn local(&self) -> &(SimpleSpan, String) {
		self.local.as_ref().unwrap_or(&self.name)
	}
}

#[derive(Clone)]
pub(crate) struct ImportDecl {
	pub external: bool,
	pub path: (SimpleSpan, String),
	pub names: Vec<ImportName>,
}

#[derive(Clone)]
pub(crate) struct RenameDecl {
	pub old: (SimpleSpan, String),
	pub from: Option<(SimpleSpan, String)>,
	pub new: (SimpleSpan, String),
}

pub(crate) struct ParsedModule {
	pub items: Vec<Item>,
	pub ranges: Vec<Option<(usize, usize)>>,
	pub aliases: Vec<AliasDecl>,
	pub imports: Vec<ImportDecl>,
	pub renames: Vec<RenameDecl>,
}

enum TopLevel {
	Item(Item, Vec<Scalar>, (usize, usize)),
	Alias(AliasDecl),
	Import(ImportDecl),
	Renamed(RenameDecl),
}

enum TableStmt {
	Attribute(TableAttribute),
	Renamed(MemberRename),
}
fn split_table_stmts(stmts: Vec<TableStmt>) -> (Vec<TableAttribute>, Vec<MemberRename>) {
	let mut attributes = Vec::new();
	let mut renames = Vec::new();
	for stmt in stmts {
		match stmt {
			TableStmt::Attribute(a) => attributes.push(a),
			TableStmt::Renamed(r) => renames.push(r),
		}
	}
	(attributes, renames)
}

enum EnumEntry {
	Item(Vec<String>, (SimpleSpan, String, Option<String>)),
	Renamed(MemberRename),
}

enum CompositeStmt {
	Attribute(CompositeAttribute),
	Renamed(MemberRename),
}

struct S {
	id: SourceId,
	module: ModuleId,
	last_inline_scalar: AtomicUsize,
	version: u32,
}
impl S {
	fn inline_scalar(&self, span: SimpleSpan, def: InlineSqlType) -> (TypeIdent, Option<Scalar>) {
		let name = format!(
			"__inline_scalar_{}",
			self.last_inline_scalar
				.fetch_add(1, sync::atomic::Ordering::Relaxed)
		);
		let ident = TypeIdent::definition((span, &name), self.module);
		let scalar = Scalar::new(
			vec!["Inlined scalar".to_owned()],
			AnnotationList {
				annotations: vec![],
				span: SimpleSpan::new(self.id, 0, 0),
			},
			TypeDefName::unchecked_new(ident, None),
			def,
			vec![ScalarAttribute::Inline],
		);
		(ident, Some(scalar))
	}
}
parser! {
grammar schema_parser() for str {
pub(super) rule root(s:&S) -> ParsedModule = _ version_directive()? _ t:toplevel(s)**_ _ {
	let mut items = Vec::new();
	let mut ranges = Vec::new();
	let mut aliases = Vec::new();
	let mut imports = Vec::new();
	let mut renames = Vec::new();
	for t in t {
		match t {
			TopLevel::Item(item, scalars, range) => {
				ranges.extend(scalars.iter().map(|_| None));
				items.extend(scalars.into_iter().map(Item::Scalar));
				ranges.push(Some(range));
				items.push(item);
			}
			TopLevel::Alias(alias) => aliases.push(alias),
			TopLevel::Import(import) => imports.push(import),
			TopLevel::Renamed(rename) => renames.push(rename),
		}
	}
	ParsedModule {
		items,
		ranges,
		aliases,
		imports,
		renames,
	}
}

rule toplevel(s:&S) -> TopLevel
= i:import(s) {TopLevel::Import(i)}
/ a:alias(s) {TopLevel::Alias(a)}
/ r:renamed(s) {TopLevel::Renamed(r)}
/ b:position!() i:item(s) e:position!() {TopLevel::Item(i.0, i.1, (b, e))}

rule renamed(s:&S) -> RenameDecl =
	docs() "@renamed" _ old:code_ident(s) _
	from:("from" _ b:position!() p:str() e:position!() _ {(SimpleSpan::new(s.id, b as u32, e as u32), p.to_owned())})?
	"to" _ new:code_ident(s) _ ";" {
	RenameDecl {
		old: (old.0, old.1.to_owned()),
		from,
		new: (new.0, new.1.to_owned()),
	}
}
rule member_renamed(s:&S) -> MemberRename =
	"@renamed" _ old:code_ident(s) _ "to" _ new:code_ident(s) {
	MemberRename {
		old: (old.0, old.1.to_owned()),
		new: (new.0, new.1.to_owned()),
	}
}
rule table_stmt(s:&S) -> TableStmt
= r:member_renamed(s) {TableStmt::Renamed(r)}
/ a:table_attribute(s) {TableStmt::Attribute(a)}
rule composite_stmt(s:&S) -> CompositeStmt
= r:member_renamed(s) {CompositeStmt::Renamed(r)}
/ a:composite_attribute(s) {CompositeStmt::Attribute(a)}

rule import_name(s:&S) -> ImportName =
	name:code_ident(s) local:(_ "as" _ l:code_ident(s) {l})? {
	ImportName {
		name: (name.0, name.1.to_owned()),
		local: local.map(|l| (l.0, l.1.to_owned())),
	}
}
rule import(s:&S) -> ImportDecl =
	docs() "import" _ external:("@external" _)?
	names:("{" _ n:import_name(s)**comma() trailing_comma() "}" _ "from" _ {n})?
	b:position!() path:str() e:position!() _ ";" {
	ImportDecl {
		external: external.is_some(),
		path: (SimpleSpan::new(s.id, b as u32, e as u32), path.to_owned()),
		names: names.unwrap_or_default(),
	}
}

rule alias(s:&S) -> AliasDecl =
	docs() "alias" _ name:code_ident(s) _ "=" _ target:code_ident(s) _ ";" {
	AliasDecl {
		name: (name.0, name.1.to_owned()),
		target: (target.0, target.1.to_owned()),
	}
}

rule item(s:&S) -> (Item, Vec<Scalar>)
= t:table(s) {(Item::Table(t.0), t.1)}
/ t:view(s) {(Item::View(t), vec![])}
/ t:enum(s) {(Item::Enum(t), vec![])}
/ t:scalar(s) {(Item::Scalar(t), vec![])}
/ t:composite(s) {(Item::Composite(t), vec![])}
/ t:mixin(s) {(Item::Mixin(t.0), t.1)}
/ t:role(s) {(Item::Role(t), vec![])}
/ t:extension(s) {(Item::Extension(t), vec![])}

rule mixin(s:&S) -> (Mixin, Vec<Scalar>) =
	docs:docs()
	annotations:annotation_list(s) _
	"@mixin" _ name:code_ident(s) _ "{" _
		mixins:("@mixin" _ f:code_ident(s) _ ";" {MixinIdent::reference(f)})**_ _
		fields:(f:table_field(s) _ ";" {f})++_ _
		stmts:(a:table_stmt(s) _ ";" {a})**_ _
		foreign_keys:(f:foreign_key(s) _ ";" {f})**_ _
	"}" _ ";" {{
	let (fields, scalars): (Vec<Column>, Vec<Option<Scalar>>) = fields.into_iter().unzip();
	let (attributes, renames) = split_table_stmts(stmts);
	let mut mixin = Mixin::new(
		docs,
		annotations,
		MixinIdent::definition(name, s.module),
		fields,
		attributes,
		foreign_keys,
		mixins,
	);
	mixin.renames = renames;
	(mixin, scalars.into_iter().flatten().collect())
}};

rule table(s:&S) -> (Table, Vec<Scalar>) =
	docs:docs()
	annotations:annotation_list(s) _
	"table" _ name:def_name(s) _ "{" _
		mixins:("@mixin" _ f:code_ident(s) _ ";" {MixinIdent::reference(f)})**_ _
		fields:(f:table_field(s) _ ";" {f})++_ _
		stmts:(a:table_stmt(s) _ ";" {a})**_ _
		foreign_keys:(f:foreign_key(s) _ ";" {f})**_ _
	"}" _ ";" {{
	let (fields, scalars): (Vec<Column>, Vec<Option<Scalar>>) = fields.into_iter().unzip();
	let (attributes, renames) = split_table_stmts(stmts);
	let mut table = Table::new(
		docs,
		annotations,
		TableDefName::definition(name, s.module),
		fields,
		attributes,
		foreign_keys,
		mixins,
	);
	table.renames = renames;
	(table, scalars.into_iter().flatten().collect())
}};

rule role_attribute(s:&S) -> RoleAttribute
= "default" _ "{" _ p:(p:permission() _ ";" {p})**_ _ "}" {RoleAttribute::Default(p)}
/ "@external" {RoleAttribute::External}
rule role(s:&S) -> Role =
	docs:docs()
	annotations:annotation_list(s) _
	"role" _ name:def_name(s) _ "{" _
		attributes:(a:role_attribute(s) _ ";" {a})**_ _
	"}" _ ";" {
	Role::new(docs, annotations, RoleDefName::definition(name, s.module), attributes)
};
rule extension_attribute(s:&S) -> ExtensionAttribute
= "version" _ v:str() {ExtensionAttribute::Version(v.to_owned())}
/ "cascade" {ExtensionAttribute::Cascade}
/ "@external" {ExtensionAttribute::External}
rule extension(s:&S) -> Extension =
	docs:docs()
	annotations:annotation_list(s) _
	"extension" _ name:def_name(s) _
		attributes:("{" _ a:(a:extension_attribute(s) _ ";" {a})**_ _ "}" _ {a})?
	";" {
	Extension::new(docs, annotations, ExtensionDefName::definition(name, s.module), attributes.unwrap_or_default())
};
rule permission() -> Permission
= "select" {Permission::Select}
/ "insert" {Permission::Insert}
/ "update" {Permission::Update}
/ "delete" {Permission::Delete}
rule role_grant(s:&S) -> RoleGrant
= "@role" _ role:code_ident(s) _ "{" _ p:(p:permission() _ ";" {p})**_ _ "}" {
	RoleGrant {
		role: RoleIdent::reference(role),
		permissions: p,
	}
}
rule policy(s:&S) -> Policy
= "@policy" _ "." _ "for" _ "(" _ role:code_ident(s) _ ")" _ name:db_ident()? _ "(" _ check:sql(s) _ ")" {
	Policy::new(name.map(DbIdent::new), RoleIdent::reference(role), check)
}

rule definition_part(s:&S) -> DefinitionPart =
	i:code_ident(s) _ "." _ j:code_ident(s) {DefinitionPart::ColumnRef(TableIdent::reference(i), ColumnIdent::member(j))}
/	i:code_ident(s) {DefinitionPart::TableRef(TableIdent::reference(i))}
rule definition(s:&S) -> Definition = parts:(
	inline(<definition_part(s)>)
/	compat_only(s, <custom_inline(<definition_part(s)>, <"$$">, <"$$">, <"${">, <"}">)>)
) {
	Definition(parts.into_iter().map(|v| match v {
		InlinePart::Text(t) => DefinitionPart::Raw(t),
		InlinePart::Template(t) => t,
	}).collect())
}
rule inline_sql_part(s:&S) -> InlineSqlTypePart =
	i:code_ident(s) {InlineSqlTypePart::TypeRef(TypeIdent::reference(i))}
rule inline_sql(s:&S) -> InlineSqlType = parts:(
	inline(<inline_sql_part(s)>)
	/ compat_only(s, <custom_inline(<inline_sql_part(s)>, <"$$">, <"$$">, <"${">, <"}">)>)
) {{
	InlineSqlType(parts.into_iter().map(|v| match v {
		InlinePart::Text(t) => InlineSqlTypePart::Raw(t),
		InlinePart::Template(t) => t,
	}).collect())
}}
/ s:compat_only(s, <str()>) {InlineSqlType(vec![InlineSqlTypePart::Raw(s.to_string())])}
rule view_attribute(s:&S) -> ViewAttribute
= "@security_definer" {ViewAttribute::SecurityDefiner}
/ g:role_grant(s) {ViewAttribute::RoleGrant(g)}
rule view(s:&S) -> View =
	docs:docs()
	annotations:annotation_list(s) _
	"view" materialized:(_ "." _ "materialized")? _ name:def_name(s) _
	attributes:("{" _ a:(a:view_attribute(s) _ ";" {a})**_ _ "}" _ {a})?
	"=" _ definition:definition(s) _ ";" {{
	let mut attributes = attributes.unwrap_or_default();
	if s.version < 2
		&& !attributes.iter().any(|a| matches!(a, ViewAttribute::SecurityDefiner))
	{
		attributes.push(ViewAttribute::SecurityDefiner);
	}
	View::new(
		docs,
		annotations,
		ViewDefName::definition(name, s.module),
		materialized.is_some(),
		attributes,
		definition,
	)
}};
rule enum(s:&S) -> Enum =
	docs:docs()
	annotations:annotation_list(s) _
	"enum" _ name:def_name(s) _ "{" _ entries:(
		r:member_renamed(s) {EnumEntry::Renamed(r)}
	/	docs:docs()
		name:def_name(s)
	{EnumEntry::Item(docs, (name.0, name.1.to_owned(), name.2.map(str::to_owned)))})++(_ ";" _) _ ";"? _ "}" _ ";" {?
	let mut items = Vec::new();
	let mut renames = Vec::new();
	for entry in entries {
		match entry {
			EnumEntry::Item(docs, (span, code, db)) => items.push(EnumItem::new(
				docs,
				EnumItemDefName::member((span, &code, db.as_deref())),
			)),
			EnumEntry::Renamed(r) => renames.push(r),
		}
	}
	if items.is_empty() {
		return Err("enum variant");
	}
	let mut en = Enum::new(docs, annotations, TypeDefName::definition(name, s.module), items);
	en.renames = renames;
	Ok(en)
};
rule scalar(s:&S) -> Scalar =
	docs:docs()
	annotations:annotation_list(s) _
	"scalar" _ name:def_name(s) _ "=" _ native:inline_sql(s) _ attributes:scalar_attribute(s)**_ ";" {
	Scalar::new(docs, annotations, TypeDefName::definition(name, s.module), native, attributes)
}
rule composite(s:&S) -> Composite =
	docs:docs()
	annotations:annotation_list(s) _
	"struct" _ name:def_name(s) _ "{" _
		fields:(f:field(s) _ ";" {f})++_ _
		stmts:(a:composite_stmt(s) _ ";" {a})**_ _
	"}" _ ";" {{
	let mut attributes = Vec::new();
	let mut renames = Vec::new();
	for stmt in stmts {
		match stmt {
			CompositeStmt::Attribute(a) => attributes.push(a),
			CompositeStmt::Renamed(r) => renames.push(r),
		}
	}
	let mut composite =
		Composite::new(docs, annotations, TypeDefName::definition(name, s.module), fields, attributes);
	composite.renames = renames;
	composite
}}
rule field(s:&S) -> Field =
	docs:docs()
	name:def_name(s) _
	t:(":" _ i:code_ident(s) _ {i})?
	n:("?" _)?
	attributes:field_attribute(s)**_ {
	Field::new(
		docs,
		DefName::member(name),
		n.is_some(),
		TypeIdent::reference(t.unwrap_or((name.0, name.1))),
		attributes
	)
}

rule maybe_inline_scalar(s:&S) -> (TypeIdent, Option<Scalar>) =
	b:position!() ty:inline_sql(s) e:position!() {s.inline_scalar(
		SimpleSpan::new(s.id, b as u32, e as u32),
		ty,
	)}
/	i:code_ident(s) { (TypeIdent::reference(i), None) }

rule table_field(s:&S) -> (Column, Option<Scalar>) =
	docs:docs()
	annotations:annotation_list(s) _
	name:def_name(s) _
	t:(":" _ i:maybe_inline_scalar(s) _ {i})?
	n:("?" _)?
	attributes:column_attribute(s)**_
	foreign_key:partial_foreign_key(s)? {
	let (ty, scalars) = match t {
		Some(v) => v,
		None => (TypeIdent::reference((name.0, name.1)), None),
	};
	(Column::new(
		DefName::member(name),
		docs,
		annotations,
		n.is_some(),
		ty,
		attributes,
		foreign_key
	), scalars)
}

rule annotation_list(s:&S) -> AnnotationList
= p:position!() list:annotation(s) ** _ {
	AnnotationList{
		annotations: list,
		span: SimpleSpan::new(s.id, p as u32, p as u32),
	}
}
rule annotation(s:&S) -> Annotation
= "#" _ name:code_ident(s) fields:(_ "(" _ f:(f:annotation_field(s)++comma() trailing_comma() {f}) _ ")" {f})? {
	Annotation {
		name: name.1.to_owned(),
		fields: fields.unwrap_or_default(),
		span: name.0,
	}
}
rule annotation_field(s:&S) -> AnnotationField
= key:code_ident(s) v:(_ "=" _ value:annotation_value() {value})? {
	AnnotationField {
		key: key.1.to_owned(),
		value: v.unwrap_or(AnnotationValue::Set),
		span: key.0,
	}
}
rule annotation_value() -> AnnotationValue
= s:str() {AnnotationValue::String(s.to_owned())}

rule scalar_attribute(s:&S) -> ScalarAttribute
= c:check(s) {ScalarAttribute::Check(c)}
/ u:unique(s) {ScalarAttribute::Unique(u)}
/ pk:primary_key(s) {ScalarAttribute::PrimaryKey(pk)}
/ d:default(s) {ScalarAttribute::Default(d)}
/ i:index(s) {ScalarAttribute::Index(i)}
/ "@inline" {ScalarAttribute::Inline}
/ "@external" {ScalarAttribute::External}
rule column_attribute(s:&S) -> ColumnAttribute
= i:index(s) {ColumnAttribute::Index(i)}
/ c:check(s) {ColumnAttribute::Check(c)}
/ u:unique(s) {ColumnAttribute::Unique(u)}
/ pk:primary_key(s) {ColumnAttribute::PrimaryKey(pk)}
/ d:default(s) {ColumnAttribute::Default(d)}
/ d:initialize_as(s) {ColumnAttribute::InitializeAs(d)}
/ p:policy(s) {ColumnAttribute::Policy(p)}
rule table_attribute(s:&S) -> TableAttribute
= c:check(s) {TableAttribute::Check(c)}
/ u:unique(s) {TableAttribute::Unique(u)}
/ pk:primary_key(s) {TableAttribute::PrimaryKey(pk)}
/ i:index(s) {TableAttribute::Index(i)}
/ p:policy(s) {TableAttribute::Policy(p)}
/ g:role_grant(s) {TableAttribute::RoleGrant(g)}
/ "@external" {TableAttribute::External}
/ "@rls.owner" {TableAttribute::RlsOwner}
/ "@rls" {TableAttribute::Rls}
rule composite_attribute(s:&S) -> CompositeAttribute
= c:check(s) {CompositeAttribute::Check(c)}
rule field_attribute(s:&S) -> FieldAttribute
= c:check(s) {FieldAttribute::Check(c)}

rule def_name(s:&S) -> (SimpleSpan, &'input str, Option<&'input str>)
= c:code_ident(s) d:(_ d:db_ident() {d})? {
	(c.0, c.1, d)
}

rule foreign_key(s:&S) -> ForeignKey
= source_fields:index_fields(s)? _ pfk:partial_foreign_key(s) {
	let mut fk = pfk.fk;
	assert!(fk.source_fields.is_none());
	fk.source_fields = source_fields;
	fk
}

rule partial_foreign_key(s:&S) -> PartialForeignKey
= name:db_ident()? _ "~" _ on_delete:("." _ a:on_delete() {a})? _ target:code_ident(s) _ target_fields:index_fields(s)? {
	PartialForeignKey {
		fk: ForeignKey::new(
			name.map(DbIdent::new),
			None,
			TableIdent::reference(target),
			target_fields,
			on_delete.unwrap_or(OnDelete::Noop),
		)
	}
}
rule on_delete() -> OnDelete
= "set_null" {OnDelete::SetNull}
/ "set_default" {OnDelete::SetDefault}
/ "restrict" {OnDelete::Restrict}
/ "noop" {OnDelete::Noop}
/ "cascade" {OnDelete::Cascade}

rule default(s:&S) -> Sql
= "@default" _ "(" _ s:sql(s) _ ")" {s}
rule initialize_as(s:&S) -> Sql
= "@initialize_as" _ "(" _ s:sql(s) _ ")" {s}
rule primary_key(s:&S) -> PrimaryKey
= "@primary_key" _ name:db_ident()? _ columns:index_fields(s)? {PrimaryKey::new(name.map(DbIdent::new), columns.unwrap_or_default())}
rule unique(s:&S) -> UniqueConstraint
= "@unique" _ name:db_ident()? _ columns:index_fields(s)? {UniqueConstraint::new(name.map(DbIdent::new), columns.unwrap_or_default())}
rule check(s:&S) -> Check
= "@check" _ name:db_ident()? _ "(" _ check:sql(s) _ ")" {Check::new(name.map(DbIdent::new), check)}
rule index(s:&S) -> Index
= "@index" _
	unq:("." _ "unique" _)?
	default_opclass:("." _ "opclass" _ "(" _ opclass:code_ident(s) _ ")" _ {OpClass(opclass.1.to_owned())})?
	using:("." _ "using" _ "(" _ using:code_ident(s) _ ")" _ {Using(using.1.to_owned())})?
	with:("." _ "with" _ "(" _ with:str_escaping() _ ")" _ {With(with)})?
	name:db_ident()? _ f:index_fields(s)? {Index::new(name.map(DbIdent::new), unq.is_some(), f.unwrap_or_default().into_iter().map(|v| (v, None)).collect(), using, default_opclass, with)}

rule index_fields(s:&S) -> Vec<ColumnIdent> = "(" _ i:code_ident(s)**comma() trailing_comma() ")" {i.into_iter().map(ColumnIdent::member).collect()}

rule code_ident(s:&S) -> (SimpleSpan, &'input str) = b:position!() n:$(['a'..='z' | 'A'..='Z'] ['a'..='z' | 'A'..='Z' | '_' | '0'..='9']*) e:position!() {(SimpleSpan::new(s.id, b as u32, e as u32), n)};
rule db_ident() -> &'input str = str()
// TODO: Replce all usages of str with str_escaping
rule str() -> &'input str = "\"" v:$((!['"' | '\''] [_])*) "\"" {v}
rule str_escaping() -> String = "\"" v:$((!['"'] ("\\\"" / [_]))*) "\"" {v.replace("\\\"", "\"")}

rule inline_part<T>(x: rule<T>, tstart:rule<()>, tend:rule<()>, end:rule<()>) -> InlinePart<T> =
	raw:$((!tstart() !tend() !end() [_])+) {InlinePart::Text(raw.to_string())}
/	v:$(tstart()) tstart() {InlinePart::Text(v.to_string())}
/	v:$(tend()) tend() {InlinePart::Text(v.to_string())}
/	v:(tstart() v:x() tend() {v}) {InlinePart::Template(v)}
/  (quiet!{tend()} / expected!("template or plain")) {unreachable!("unexpected inline part")}
rule custom_inline<T>(x: rule<T>, start:rule<()>, end:rule<()>, tstart:rule<()>, tend:rule<()>) -> Vec<InlinePart<T>> =
	start() parts:inline_part(<x()>, <tstart()>, <tend()>, <end()>)* end() { parts }

rule inline<T>(x: rule<T>) -> Vec<InlinePart<T>> =
	custom_inline(<x()>, <"sql\"\"\"">, <"\"\"\"">, <"{">, <"}">)
/	custom_inline(<x()>, <"sql\"">, <"\"">, <"{">, <"}">)
rule compat_only<T>(s:&S, x: rule<T>) -> T =
	v:x() {? if s.version <= 1 {
		Ok(v)
	} else {
		Err("syntax is deprecated and only allowed for old schemas")
	}}

rule version_directive() = "@!schema_version" _ ['0'..='9']+ _ ";"


rule sqlexpr(s:&S) -> Sql = "(" s:sql(s) ")" {s};
rule sql(s:&S) -> Sql
= precedence! {
	a:(@) _ "||" _ b:@ {Sql::BinOp(h(a), SqlOp::Or, h(b))}
	--
	a:(@) _ "&&" _ b:@ {Sql::BinOp(h(a), SqlOp::And, h(b))}
	--
	a:(@) _ "===" _ b:@ {Sql::BinOp(h(a), SqlOp::SEq, h(b))}
	a:(@) _ "!==" _ b:@ {Sql::BinOp(h(a), SqlOp::SNe, h(b))}
	a:(@) _ "==" _ b:@ {Sql::BinOp(h(a), SqlOp::Eq, h(b))}
	a:(@) _ "!=" _ b:@ {Sql::BinOp(h(a), SqlOp::Ne, h(b))}
	a:(@) _ "~~" _ b:@ {Sql::BinOp(h(a), SqlOp::Like, h(b))}
	--
	a:(@) _ "<" _ b:@ {Sql::BinOp(h(a), SqlOp::Lt, h(b))}
	a:(@) _ ">" _ b:@ {Sql::BinOp(h(a), SqlOp::Gt, h(b))}
	a:(@) _ "<=" _ b:@ {Sql::BinOp(h(a), SqlOp::Le, h(b))}
	a:(@) _ ">=" _ b:@ {Sql::BinOp(h(a), SqlOp::Ge, h(b))}
	--
	a:(@) _ "+" _ b:@ {Sql::BinOp(h(a), SqlOp::Plus, h(b))}
	a:(@) _ "-" _ b:@ {Sql::BinOp(h(a), SqlOp::Minus, h(b))}
	--
	a:(@) _ "*" _ b:@ {Sql::BinOp(h(a), SqlOp::Mul, h(b))}
	a:(@) _ "/" _ b:@ {Sql::BinOp(h(a), SqlOp::Div, h(b))}
	a:(@) _ "%" _ b:@ {Sql::BinOp(h(a), SqlOp::Mod, h(b))}
	--
	"-" _ b:@ {Sql::UnOp(SqlUnOp::Minus, h(b))}
	"+" _ b:@ {Sql::UnOp(SqlUnOp::Plus, h(b))}
	"!" _ b:@ {Sql::UnOp(SqlUnOp::Not, h(b))}
	--
	a:(@) _ "::" _ ty:code_ident(s) {Sql::Cast(h(a), TypeIdent::reference(ty))}
	a:(@) _ "." _ f:code_ident(s) {Sql::GetField(h(a), FieldIdent::member(f))}
	--
	e:sql_basic(s) {e}
}
rule sql_basic(s:&S) -> Sql
= "(" _ e:(e:sql(s) _ "," _ {e})*<2,> _ ")" {Sql::Tuple(e)}
/ "(" _ e:sql(s) _ "," _ ")" {Sql::Tuple(vec![e])}
/ "(" _ e:sql(s) _ ")" {Sql::Parened(h(e))}
/ "(" _ ")" {Sql::Tuple(vec![])}
/ "_" {Sql::Placeholder}
/ "if" _ c:sql(s) _ "then" _ then:sql(s) _ "else" _ else_:sql(s) {Sql::If(h(c), h(then), h(else_))}
/ "null" {Sql::Null}
/ "true" {Sql::Boolean(true)}
/ "false" {Sql::Boolean(false)}
/ i:code_ident(s) _ "(" _ e:sql(s)**comma() trailing_comma() ")" {Sql::Call(DbProcedure::new(i.1), e)}
/ i:code_ident(s) {Sql::Ident(ColumnIdent::member(i))}
/ s:str() {Sql::String(s.to_owned())}
/ n:$(['0'..='9']+) {Sql::Number(n.parse().unwrap())}

rule trailing_comma() = _ ","? _;
rule comma() = _ "," _;
rule _() = ([' ' | '\t' | '\n'] / ("///" (!['\n'] [_])+ ['\n']))*;
rule docs() -> Vec<String> = l:("//" s:$((!['\n'] [_])+) "\n" _ {s.to_string()})* {l}
}
}

#[derive(thiserror::Error, Debug)]
pub enum ParsingError {
	#[error("parser error: {0}")]
	Peg(#[from] ParseError<LineCol>),
}

type Result<T> = result::Result<T, ()>;

fn scan_version_directive(v: &str) -> Option<(u32, u32, u32)> {
	let start = v.find("@!schema_version")?;
	let rest = &v[start + "@!schema_version".len()..];
	let digits = rest.trim_start();
	let num: String = digits.chars().take_while(char::is_ascii_digit).collect();
	let end = start + "@!schema_version".len() + (rest.len() - digits.len()) + num.len();
	let version = num.parse().ok()?;
	Some((version, start as u32, end as u32))
}

pub(crate) fn source_version(
	v: &str,
	span: SourceId,
	schema_version: SchemaVersion,
	report: &mut Report,
) -> Result<u32> {
	Ok(match schema_version {
		SchemaVersion::Current => {
			if let Some((version, start, end)) = scan_version_directive(v)
				&& version != LATEST_SCHEMA_VERSION
			{
				report
					.error(format!(
						"expected schema version {version}, but this version of immigrant supports version {LATEST_SCHEMA_VERSION}"
					))
					.annotate("declared here", SimpleSpan::new(span, start, end));
				return Err(());
			}
			LATEST_SCHEMA_VERSION
		}
		SchemaVersion::Stored(version) => {
			if version == 0 || version > LATEST_SCHEMA_VERSION {
				report.error(format!(
					"unsupported schema version {version}, this version of immigrant supports versions 1..={LATEST_SCHEMA_VERSION}"
				));
				return Err(());
			}
			version
		}
	})
}

pub(crate) fn parse_module(
	v: &str,
	span: SourceId,
	module: ModuleId,
	version: u32,
	report: &mut Report,
) -> Result<ParsedModule> {
	schema_parser::root(
		v,
		&S {
			id: span,
			module,
			last_inline_scalar: AtomicUsize::default(),
			version,
		},
	)
	.map_err(|e| {
		report.error("parse error").annotate(
			e.to_string(),
			SimpleSpan::new(span, e.location.offset as u32, e.location.offset as u32),
		);
	})
}

pub fn parse(
	v: &str,
	schema_version: SchemaVersion,
	opts: &SchemaProcessOptions,
	rn: &mut RenameMap,
	report: &mut Report,
) -> Result<Schema> {
	let sources = BTreeMap::from([(ROOT_MODULE.to_owned(), v.to_owned())]);
	let root = ResolvedSpec::root(ROOT_MODULE).expect("valid root");
	parse_project(&[root], &sources, schema_version, opts, rn, report).map(|p| p.schema)
}
