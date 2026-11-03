use std::collections::{HashMap, HashSet};

use crate::{
	HasIdent,
	column::{Column, ColumnAttribute},
	composite::{CompositeAttribute, FieldAttribute},
	diagnostics::Report,
	ids::{Ident, ModuleId, NameId, intern_name, name_of},
	names::{MixinIdent, QualifiedName, TypeIdent},
	parser::{AliasDecl, ImportDecl, RenameDecl},
	role::Policy,
	root::Item,
	scalar::{InlineSqlTypePart, ScalarAttribute},
	source::ResolvedSpec,
	span::SimpleSpan,
	sql::{Sql, SqlVisitor},
	table::{ForeignKey, TableAttribute},
	view::{DefinitionPart, ViewAttribute},
};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Namespace {
	Item,
	Mixin,
	Role,
	Extension,
}
impl Namespace {
	pub(crate) const ALL: [Namespace; 4] = [
		Namespace::Item,
		Namespace::Mixin,
		Namespace::Role,
		Namespace::Extension,
	];
}

#[derive(Clone, Copy, Debug)]
pub struct Resolution {
	pub reference: SimpleSpan,
	pub definition: SimpleSpan,
}

#[derive(Clone, Copy)]
struct Target {
	module: ModuleId,
	name: NameId,
	span: SimpleSpan,
}

enum Lookup {
	Found(Target),
	Missing,
	Broken,
}

struct Alias {
	name_span: SimpleSpan,
	target: NameId,
	target_span: SimpleSpan,
}

struct Import {
	local_span: SimpleSpan,
	from: ModuleId,
	specifier: NameId,
	specifier_span: SimpleSpan,
}

pub(crate) struct Resolver<'r> {
	decls: HashMap<(ModuleId, Namespace, NameId), SimpleSpan>,
	aliases: HashMap<(ModuleId, NameId), Alias>,
	imports: HashMap<(ModuleId, NameId), Import>,
	broken: HashSet<(ModuleId, NameId)>,
	resolutions: Vec<Resolution>,
	used_bindings: Vec<(usize, ModuleId, NameId)>,
	current_item: usize,
	report: &'r mut Report,
	failed: bool,
}

pub(crate) fn item_namespace(item: &Item) -> Namespace {
	match item {
		Item::Table(_) | Item::Enum(_) | Item::Scalar(_) | Item::Composite(_) | Item::View(_) => {
			Namespace::Item
		}
		Item::Mixin(_) => Namespace::Mixin,
		Item::Role(_) => Namespace::Role,
		Item::Extension(_) => Namespace::Extension,
	}
}

pub(crate) fn item_ident(item: &Item) -> Ident<()> {
	match item {
		Item::Table(v) => v.id().erase(),
		Item::Mixin(v) => v.id().erase(),
		Item::Enum(v) => v.id().erase(),
		Item::Scalar(v) => v.id().erase(),
		Item::Composite(v) => v.id().erase(),
		Item::View(v) => v.id().erase(),
		Item::Role(v) => v.id().erase(),
		Item::Extension(v) => v.id().erase(),
	}
}

pub(crate) fn item_module(item: &Item) -> ModuleId {
	item_ident(item)
		.module()
		.expect("item definitions belong to a module")
}

impl<'r> Resolver<'r> {
	pub(crate) fn new(report: &'r mut Report) -> Self {
		Self {
			decls: HashMap::new(),
			aliases: HashMap::new(),
			imports: HashMap::new(),
			broken: HashSet::new(),
			resolutions: Vec::new(),
			used_bindings: Vec::new(),
			current_item: 0,
			report,
			failed: false,
		}
	}

	pub(crate) fn declare_items(&mut self, items: &[Item]) {
		for item in items {
			let id = item_ident(item);
			let module = item_module(item);
			self.decls
				.entry((module, item_namespace(item), id.name_id()))
				.or_insert(id.span());
		}
	}

	fn is_bound(&self, module: ModuleId, name: NameId) -> Option<SimpleSpan> {
		for ns in Namespace::ALL {
			if let Some(span) = self.decls.get(&(module, ns, name)) {
				return Some(*span);
			}
		}
		if let Some(import) = self.imports.get(&(module, name)) {
			return Some(import.local_span);
		}
		self.aliases.get(&(module, name)).map(|a| a.name_span)
	}

	fn check_unbound(
		&mut self,
		module: ModuleId,
		name: NameId,
		local: &(SimpleSpan, String),
	) -> bool {
		let Some(previous) = self.is_bound(module, name) else {
			return true;
		};
		self.report
			.error(format!("`{}` is already defined in this module", local.1))
			.annotate("redefined here", local.0)
			.annotate("previously defined here", previous);
		self.failed = true;
		false
	}

	pub(crate) fn declare_imports(
		&mut self,
		module: ModuleId,
		imports: Vec<(ImportDecl, Option<ModuleId>)>,
	) {
		for (import, from) in imports {
			let Some(from) = from else {
				continue;
			};
			for name in import.names {
				let local = name.local();
				let local_id = intern_name(&local.1);
				if !self.check_unbound(module, local_id, local) {
					continue;
				}
				self.imports.insert(
					(module, local_id),
					Import {
						local_span: local.0,
						from,
						specifier: intern_name(&name.name.1),
						specifier_span: name.name.0,
					},
				);
			}
		}
	}

	pub(crate) fn check_imports(&mut self) {
		let mut keys: Vec<_> = self.imports.keys().copied().collect();
		keys.sort();
		for key in keys {
			let import = &self.imports[&key];
			if Namespace::ALL.iter().any(|ns| {
				self.decls
					.contains_key(&(import.from, *ns, import.specifier))
			}) {
				continue;
			}
			self.report
				.error(format!(
					"module {:?} does not declare `{}`",
					import.from.path(),
					name_of(import.specifier)
				))
				.annotate("imported here", import.specifier_span);
			self.failed = true;
			self.broken.insert(key);
		}
	}

	pub(crate) fn declare_aliases(&mut self, module: ModuleId, aliases: Vec<AliasDecl>) {
		for alias in aliases {
			let name = intern_name(&alias.name.1);
			if !self.check_unbound(module, name, &alias.name) {
				continue;
			}
			self.aliases.insert(
				(module, name),
				Alias {
					name_span: alias.name.0,
					target: intern_name(&alias.target.1),
					target_span: alias.target.0,
				},
			);
		}
	}

	fn lookup(
		&self,
		module: ModuleId,
		ns: Namespace,
		name: NameId,
		visited: &mut HashSet<(ModuleId, NameId)>,
		trace: &mut Vec<(ModuleId, NameId)>,
	) -> Lookup {
		if let Some(span) = self.decls.get(&(module, ns, name)) {
			return Lookup::Found(Target {
				module,
				name,
				span: *span,
			});
		}
		if let Some(import) = self.imports.get(&(module, name)) {
			if self.broken.contains(&(module, name)) {
				return Lookup::Broken;
			}
			trace.push((module, name));
			return match self.decls.get(&(import.from, ns, import.specifier)) {
				Some(span) => Lookup::Found(Target {
					module: import.from,
					name: import.specifier,
					span: *span,
				}),
				None => Lookup::Missing,
			};
		}
		let Some(alias) = self.aliases.get(&(module, name)) else {
			return Lookup::Missing;
		};
		if self.broken.contains(&(module, name)) || !visited.insert((module, name)) {
			return Lookup::Broken;
		}
		trace.push((module, name));
		self.lookup(module, ns, alias.target, visited, trace)
	}

	pub(crate) fn check_aliases(&mut self) {
		let mut keys: Vec<_> = self.aliases.keys().copied().collect();
		keys.sort();
		for (module, name) in keys {
			let alias = &self.aliases[&(module, name)];
			let (name_span, target, target_span) =
				(alias.name_span, alias.target, alias.target_span);
			let mut found = false;
			let mut cycle = false;
			for ns in Namespace::ALL {
				let mut visited = HashSet::from([(module, name)]);
				match self.lookup(module, ns, target, &mut visited, &mut Vec::new()) {
					Lookup::Found(_) => found = true,
					Lookup::Missing => {}
					Lookup::Broken => cycle = true,
				}
			}
			if cycle {
				self.report
					.error("alias cycle")
					.annotate("this alias refers to itself", name_span);
			} else if !found {
				self.report
					.error(format!("unknown name `{}`", name_of(target)))
					.annotate("aliased here", target_span);
			} else {
				continue;
			}
			self.failed = true;
			self.broken.insert((module, name));
		}
	}

	pub(crate) fn resolve_renames(
		&mut self,
		module: ModuleId,
		path: &ResolvedSpec,
		renames: &[RenameDecl],
	) -> Vec<(QualifiedName, QualifiedName)> {
		let mut out = Vec::new();
		for rename in renames {
			let old_module = match &rename.from {
				None => module,
				Some((span, spec)) => match path.resolve(spec) {
					Ok(path) => ModuleId::new(path.as_str()),
					Err(e) => {
						self.report.error(e).annotate("referenced here", *span);
						self.failed = true;
						continue;
					}
				},
			};
			let old = (Some(old_module.path()), rename.old.1.clone());
			let name = intern_name(&rename.new.1);
			let mut found = false;
			for ns in Namespace::ALL {
				let lookup = self.lookup(module, ns, name, &mut HashSet::new(), &mut Vec::new());
				if let Lookup::Found(target) = lookup {
					found = true;
					let new = (Some(target.module.path()), name_of(target.name));
					if !out.contains(&(new.clone(), old.clone())) {
						out.push((new, old.clone()));
					}
				}
			}
			if !found {
				self.report
					.error(format!("unknown name `{}`", rename.new.1))
					.annotate("renamed here", rename.new.0);
				self.failed = true;
			}
		}
		out
	}

	pub(crate) fn resolve_items(&mut self, items: &mut [Item]) {
		for (idx, item) in items.iter_mut().enumerate() {
			self.current_item = idx;
			walk_item(item, self);
		}
	}

	pub(crate) fn finish(self) -> Result<Resolved, ()> {
		if self.failed {
			return Err(());
		}
		Ok(Resolved {
			resolutions: self.resolutions,
			used_bindings: self.used_bindings,
		})
	}
}

impl RefVisitor for Resolver<'_> {
	fn visit<K>(
		&mut self,
		module: ModuleId,
		ns: Namespace,
		what: &'static str,
		ident: &mut Ident<K>,
	) {
		if ident.is_resolved() {
			return;
		}
		let mut trace = Vec::new();
		match self.lookup(module, ns, ident.name_id(), &mut HashSet::new(), &mut trace) {
			Lookup::Found(target) => {
				ident.set_resolved(Some(target.module), target.name);
				self.resolutions.push(Resolution {
					reference: ident.span(),
					definition: target.span,
				});
				self.used_bindings.extend(
					trace
						.into_iter()
						.map(|(module, name)| (self.current_item, module, name)),
				);
			}
			Lookup::Missing => {
				self.report
					.error(format!("unknown {what} `{}`", ident.name()))
					.annotate("referenced here", ident.span());
				self.failed = true;
			}
			Lookup::Broken => {
				self.failed = true;
			}
		}
	}
}

pub(crate) struct Resolved {
	pub resolutions: Vec<Resolution>,
	pub used_bindings: Vec<(usize, ModuleId, NameId)>,
}

pub(crate) trait RefVisitor {
	fn visit<K>(
		&mut self,
		module: ModuleId,
		ns: Namespace,
		what: &'static str,
		ident: &mut Ident<K>,
	);
}

fn walk_sql(module: ModuleId, sql: &mut Sql, v: &mut impl RefVisitor) {
	struct Types<'a, V> {
		v: &'a mut V,
		module: ModuleId,
	}
	impl<V: RefVisitor> SqlVisitor for Types<'_, V> {
		fn handle_type(&mut self, ty: &mut TypeIdent) {
			self.v.visit(self.module, Namespace::Item, "type", ty);
		}
	}
	sql.visit(&mut Types { v, module });
}

fn walk_policy(module: ModuleId, policy: &mut Policy, v: &mut impl RefVisitor) {
	v.visit(module, Namespace::Role, "role", &mut policy.role);
	walk_sql(module, &mut policy.check, v);
}

fn walk_column(module: ModuleId, column: &mut Column, v: &mut impl RefVisitor) {
	v.visit(module, Namespace::Item, "type", &mut column.ty);
	for attr in &mut column.attributes {
		match attr {
			ColumnAttribute::Check(c) => walk_sql(module, &mut c.check, v),
			ColumnAttribute::Default(s) | ColumnAttribute::InitializeAs(s) => {
				walk_sql(module, s, v)
			}
			ColumnAttribute::Policy(p) => walk_policy(module, p, v),
			ColumnAttribute::Unique(_)
			| ColumnAttribute::PrimaryKey(_)
			| ColumnAttribute::Index(_) => {}
		}
	}
	if let Some(fk) = &mut column.foreign_key {
		v.visit(module, Namespace::Item, "table", &mut fk.fk.target);
	}
}

fn walk_table_like(
	module: ModuleId,
	columns: &mut [Column],
	attributes: &mut [TableAttribute],
	foreign_keys: &mut [ForeignKey],
	mixins: &mut [MixinIdent],
	v: &mut impl RefVisitor,
) {
	for mixin in mixins {
		v.visit(module, Namespace::Mixin, "mixin", mixin);
	}
	for column in columns {
		walk_column(module, column, v);
	}
	for attr in attributes {
		match attr {
			TableAttribute::Check(c) => walk_sql(module, &mut c.check, v),
			TableAttribute::Policy(p) => walk_policy(module, p, v),
			TableAttribute::RoleGrant(g) => v.visit(module, Namespace::Role, "role", &mut g.role),
			TableAttribute::Unique(_)
			| TableAttribute::PrimaryKey(_)
			| TableAttribute::Index(_)
			| TableAttribute::Rls
			| TableAttribute::RlsOwner
			| TableAttribute::External => {}
		}
	}
	for fk in foreign_keys {
		v.visit(module, Namespace::Item, "table", &mut fk.target);
	}
}

pub(crate) fn walk_item(item: &mut Item, v: &mut impl RefVisitor) {
	let module = item_module(item);
	match item {
		Item::Table(t) => walk_table_like(
			module,
			&mut t.columns,
			&mut t.attributes,
			&mut t.foreign_keys,
			&mut t.mixins,
			v,
		),
		Item::Mixin(m) => walk_table_like(
			module,
			&mut m.columns,
			&mut m.attributes,
			&mut m.foreign_keys,
			&mut m.mixins,
			v,
		),
		Item::Scalar(s) => {
			for part in &mut s.native.0 {
				if let InlineSqlTypePart::TypeRef(t) = part {
					v.visit(module, Namespace::Item, "type", t);
				}
			}
			for attr in &mut s.attributes {
				match attr {
					ScalarAttribute::Default(d) => walk_sql(module, d, v),
					ScalarAttribute::Check(c) => walk_sql(module, &mut c.check, v),
					ScalarAttribute::PrimaryKey(_)
					| ScalarAttribute::Unique(_)
					| ScalarAttribute::Index(_)
					| ScalarAttribute::Inline
					| ScalarAttribute::External => {}
				}
			}
		}
		Item::Composite(c) => {
			for field in &mut c.fields {
				v.visit(module, Namespace::Item, "type", &mut field.ty);
				for attr in &mut field.attributes {
					match attr {
						FieldAttribute::Check(c) => walk_sql(module, &mut c.check, v),
					}
				}
			}
			for attr in &mut c.attributes {
				match attr {
					CompositeAttribute::Check(c) => walk_sql(module, &mut c.check, v),
				}
			}
		}
		Item::View(view) => {
			for part in &mut view.definition.0 {
				match part {
					DefinitionPart::TableRef(t) | DefinitionPart::ColumnRef(t, _) => {
						v.visit(module, Namespace::Item, "table or view", t)
					}
					DefinitionPart::Raw(_) => {}
				}
			}
			for attr in &mut view.attributes {
				match attr {
					ViewAttribute::RoleGrant(g) => {
						v.visit(module, Namespace::Role, "role", &mut g.role)
					}
					ViewAttribute::SecurityDefiner => {}
				}
			}
		}
		Item::Enum(_) | Item::Role(_) | Item::Extension(_) => {}
	}
}

#[cfg(test)]
mod tests {
	use crate::{
		diagnostics::Report,
		parser::{SchemaVersion, parse},
		process::NamingConvention,
		root::SchemaProcessOptions,
		uid::RenameMap,
	};

	fn errors(src: &str) -> Vec<String> {
		let mut report = Report::new();
		let result = parse(
			src,
			SchemaVersion::Current,
			&SchemaProcessOptions {
				generator_supports_domain: true,
				naming_convention: NamingConvention::Postgres,
			},
			&mut RenameMap::new(),
			&mut report,
		);
		assert!(
			result.is_err() || report.is_error(),
			"schema should fail to resolve"
		);
		report.parts.into_iter().map(|p| p.msg).collect()
	}

	#[test]
	fn unknown_type() {
		assert_eq!(errors("table A { id; };"), vec!["unknown type `id`"]);
	}

	#[test]
	fn unknown_mixin() {
		assert_eq!(
			errors(r#"scalar id = sql"INTEGER"; table A { @mixin M; id; };"#),
			vec!["unknown mixin `M`"]
		);
	}

	#[test]
	fn unknown_fk_target_and_role() {
		assert_eq!(
			errors(r#"scalar id = sql"INTEGER"; table A { id ~ B; @role R { select; }; };"#),
			vec!["unknown table `B`", "unknown role `R`"]
		);
	}

	#[test]
	fn unknown_cast_type() {
		assert_eq!(
			errors(r#"scalar id = sql"INTEGER" @check (_::nope > 0);"#),
			vec!["unknown type `nope`"]
		);
	}

	#[test]
	fn alias_unknown_target() {
		assert_eq!(errors("alias a = b;"), vec!["unknown name `b`"]);
	}

	#[test]
	fn alias_cycle() {
		assert_eq!(
			errors(r#"alias a = b; alias b = a; table T { x: a; };"#),
			vec!["alias cycle", "alias cycle"]
		);
	}

	#[test]
	fn rename_errors() {
		assert_eq!(errors("@renamed a to b;"), vec!["unknown name `b`"]);
		assert_eq!(
			errors(r#"scalar x = sql"INTEGER"; @renamed a from "x.schema" to x;"#),
			vec![
				"import path \"x.schema\" should be relative, starting with \"./\" or \"../\", or point to a package, starting with \"@\""
			]
		);
		assert_eq!(
			errors(r#"scalar x = sql"INTEGER"; table T { x; @renamed y to z; };"#),
			vec!["unknown column `z`"]
		);
		assert_eq!(
			errors(r#"enum E { a; @renamed b to c; };"#),
			vec!["unknown enum variant `c`"]
		);
	}

	#[test]
	fn alias_clash() {
		assert_eq!(
			errors(r#"scalar id = sql"INTEGER"; scalar other = sql"TEXT"; alias id = other;"#),
			vec!["`id` is already defined in this module"]
		);
	}
}
