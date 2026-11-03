use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::{
	column::ColumnAttribute,
	diagnostics::Report,
	ids::{Ident, ModuleId, NameId, in_allocator, intern_name},
	names::QualifiedName,
	parser::{ParsedModule, SchemaVersion, parse_module, source_version},
	resolve::{
		Namespace, RefVisitor, Resolution, Resolver, item_ident, item_module, item_namespace,
		walk_item,
	},
	role::RoleAttribute,
	root::{Item, Schema, SchemaMeta, SchemaProcessOptions},
	source::{ResolvedSpec, SourceLoader},
	span::{SimpleSpan, register_source},
	table::TableAttribute,
	uid::{HasUid, RenameMap},
	view::ViewAttribute,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModuleRole {
	Managed,
	External,
}

#[derive(Debug)]
pub struct Project {
	pub schema: Schema,
	pub modules: BTreeMap<ResolvedSpec, ModuleRole>,
	pub sources: BTreeMap<String, String>,
	pub resolutions: Vec<Resolution>,
}

struct LoadedModule {
	path: ResolvedSpec,
	id: ModuleId,
	text: String,
	parsed: ParsedModule,
	targets: Vec<Option<usize>>,
}

fn load_modules(
	roots: &[ResolvedSpec],
	loader: &dyn SourceLoader,
	schema_version: SchemaVersion,
	report: &mut Report,
) -> Result<Vec<LoadedModule>, ()> {
	let mut failed = false;
	let mut modules: Vec<LoadedModule> = Vec::new();
	let mut index: HashMap<ResolvedSpec, usize> = HashMap::new();
	let mut unavailable: HashSet<ResolvedSpec> = HashSet::new();
	let mut pending: VecDeque<(ResolvedSpec, Option<SimpleSpan>)> =
		roots.iter().map(|root| (root.clone(), None)).collect();
	let mut wanted: Vec<(usize, usize, ResolvedSpec)> = Vec::new();

	while let Some((path, requested_at)) = pending.pop_front() {
		if index.contains_key(&path) || unavailable.contains(&path) {
			continue;
		}
		let text = match loader.load(&path) {
			Ok(text) => text,
			Err(e) => {
				let mut error = report.error(format!("failed to load {:?}: {e}", path.as_str()));
				if let Some(span) = requested_at {
					error.annotate("imported here", span);
				}
				failed = true;
				unavailable.insert(path);
				continue;
			}
		};
		let source = register_source(path.to_string(), text.clone());
		let Ok(version) = source_version(&text, source, schema_version, report) else {
			failed = true;
			unavailable.insert(path);
			continue;
		};
		let id = ModuleId::new(path.as_str());
		let Ok(parsed) = parse_module(&text, source, id, version, report) else {
			failed = true;
			unavailable.insert(path);
			continue;
		};
		let module_idx = modules.len();
		for (import_idx, import) in parsed.imports.iter().enumerate() {
			match path.resolve(&import.path.1) {
				Ok(target) => {
					pending.push_back((target.clone(), Some(import.path.0)));
					wanted.push((module_idx, import_idx, target));
				}
				Err(e) => {
					report.error(e).annotate("imported here", import.path.0);
					failed = true;
				}
			}
		}
		let targets = vec![None; parsed.imports.len()];
		index.insert(path.clone(), module_idx);
		modules.push(LoadedModule {
			path,
			id,
			text,
			parsed,
			targets,
		});
	}
	for (module_idx, import_idx, target) in wanted {
		modules[module_idx].targets[import_idx] = index.get(&target).copied();
	}
	if failed {
		return Err(());
	}
	Ok(modules)
}

fn module_roles(modules: &[LoadedModule], roots: &[ResolvedSpec]) -> Vec<ModuleRole> {
	let mut roles = vec![ModuleRole::External; modules.len()];
	let mut pending: Vec<usize> = roots
		.iter()
		.filter_map(|root| modules.iter().position(|m| &m.path == root))
		.collect();
	while let Some(idx) = pending.pop() {
		if roles[idx] == ModuleRole::Managed {
			continue;
		}
		roles[idx] = ModuleRole::Managed;
		let module = &modules[idx];
		for (import, target) in module.parsed.imports.iter().zip(&module.targets) {
			if let Some(target) = target
				&& !import.external
			{
				pending.push(*target);
			}
		}
	}
	roles
}

pub fn parse_project(
	roots: &[ResolvedSpec],
	loader: &dyn SourceLoader,
	schema_version: SchemaVersion,
	opts: &SchemaProcessOptions,
	rn: &mut RenameMap,
	report: &mut Report,
) -> Result<Project, ()> {
	let mut modules = in_allocator(|| load_modules(roots, loader, schema_version, report))?;
	let roles = module_roles(&modules, roots);

	for (module, role) in modules.iter_mut().zip(&roles) {
		if *role == ModuleRole::External {
			module.parsed.items.iter_mut().for_each(strip_external);
		}
	}

	let mut items = Vec::new();
	let mut item_ranges = Vec::new();
	let mut item_modules = Vec::new();
	let mut resolver = Resolver::new(report);
	for module in &modules {
		resolver.declare_items(&module.parsed.items);
	}
	let ids: Vec<ModuleId> = modules.iter().map(|m| m.id).collect();
	for (idx, module) in modules.iter_mut().enumerate() {
		let imports = module
			.parsed
			.imports
			.iter()
			.cloned()
			.zip(module.targets.iter().map(|t| t.map(|t| ids[t])))
			.collect();
		resolver.declare_imports(module.id, imports);
		resolver.declare_aliases(module.id, module.parsed.aliases.clone());
		item_modules.extend(module.parsed.items.iter().map(|_| idx));
		item_ranges.append(&mut module.parsed.ranges);
		items.append(&mut module.parsed.items);
	}
	resolver.check_imports();
	resolver.check_aliases();
	resolver.resolve_items(&mut items);
	let mut renames: HashMap<QualifiedName, Vec<QualifiedName>> = HashMap::new();
	for (module, role) in modules.iter().zip(&roles) {
		if *role != ModuleRole::Managed {
			continue;
		}
		for (new, old) in resolver.resolve_renames(module.id, &module.path, &module.parsed.renames)
		{
			renames.entry(new).or_default().push(old);
		}
	}
	let resolved = resolver.finish()?;

	let in_closure = stub_closure(&modules, &roles, &mut items, &item_modules);

	let mut sources = BTreeMap::new();
	let mut module_roles = BTreeMap::new();
	for (idx, (module, role)) in modules.iter().zip(&roles).enumerate() {
		module_roles.insert(module.path.clone(), *role);
		match role {
			ModuleRole::Managed => {
				sources.insert(module.path.to_string(), module.text.clone());
			}
			ModuleRole::External => {
				if let Some(slice) = slice_module(
					idx,
					module,
					&in_closure,
					&item_modules,
					&item_ranges,
					&resolved.used_bindings,
				) {
					sources.insert(module.path.to_string(), slice);
				}
			}
		}
	}

	let mut stubs = HashSet::new();
	let items = items
		.into_iter()
		.zip(item_modules)
		.enumerate()
		.filter_map(|(idx, (item, module))| {
			if roles[module] == ModuleRole::Managed {
				return Some(item);
			}
			if !in_closure.contains(&idx) {
				return None;
			}
			if !matches!(item, Item::Mixin(_)) {
				stubs.insert(item.uid());
			}
			Some(item)
		})
		.collect();

	let external_modules = modules
		.iter()
		.zip(&roles)
		.filter(|(_, role)| **role == ModuleRole::External)
		.map(|(m, _)| m.id)
		.collect();
	let mut schema = Schema(
		items,
		SchemaMeta {
			stubs,
			external_modules,
			renames,
		},
	);
	schema.process(opts, rn, report);
	// if report.is_error() {
	// 	return Err(None)
	// }
	Ok(Project {
		schema,
		modules: module_roles,
		sources,
		resolutions: resolved.resolutions,
	})
}

fn strip_external(item: &mut Item) {
	match item {
		Item::Table(t) => {
			for column in &mut t.columns {
				column.foreign_key = None;
				column.attributes.retain(|a| {
					matches!(
						a,
						ColumnAttribute::PrimaryKey(_) | ColumnAttribute::Unique(_)
					)
				});
			}
			t.attributes
				.retain(|a| matches!(a, TableAttribute::PrimaryKey(_) | TableAttribute::Unique(_)));
			t.foreign_keys.clear();
		}
		Item::View(v) => v
			.attributes
			.retain(|a| !matches!(a, ViewAttribute::RoleGrant(_))),
		Item::Role(r) => r
			.attributes
			.retain(|a| !matches!(a, RoleAttribute::Default(_))),
		Item::Mixin(_)
		| Item::Enum(_)
		| Item::Scalar(_)
		| Item::Composite(_)
		| Item::Extension(_) => {}
	}
}

struct Targets(Vec<(ModuleId, Namespace, NameId)>);
impl RefVisitor for Targets {
	fn visit<K>(
		&mut self,
		_module: ModuleId,
		ns: Namespace,
		_what: &'static str,
		ident: &mut Ident<K>,
	) {
		let module = ident.module().expect("references point to module items");
		self.0.push((module, ns, ident.name_id()));
	}
}

fn stub_closure(
	modules: &[LoadedModule],
	roles: &[ModuleRole],
	items: &mut [Item],
	item_modules: &[usize],
) -> HashSet<usize> {
	let mut decls: HashMap<(ModuleId, Namespace, NameId), Vec<usize>> = HashMap::new();
	for (idx, item) in items.iter().enumerate() {
		let key = (
			item_module(item),
			item_namespace(item),
			item_ident(item).name_id(),
		);
		decls.entry(key).or_default().push(idx);
	}

	let mut pending = Vec::new();
	for (module, role) in modules.iter().zip(roles) {
		if *role != ModuleRole::Managed {
			continue;
		}
		for (import, target) in module.parsed.imports.iter().zip(&module.targets) {
			let Some(target) = *target else {
				continue;
			};
			if roles[target] != ModuleRole::External {
				continue;
			}
			for name in &import.names {
				let name = intern_name(&name.name.1);
				for ns in Namespace::ALL {
					if let Some(found) = decls.get(&(modules[target].id, ns, name)) {
						pending.extend(found.iter().copied());
					}
				}
			}
		}
	}

	let mut closure = HashSet::new();
	while let Some(idx) = pending.pop() {
		if roles[item_modules[idx]] != ModuleRole::External || !closure.insert(idx) {
			continue;
		}
		let mut targets = Targets(Vec::new());
		walk_item(&mut items[idx], &mut targets);
		for key in targets.0 {
			if let Some(found) = decls.get(&key) {
				pending.extend(found.iter().copied());
			}
		}
	}
	closure
}

fn slice_module(
	module_idx: usize,
	module: &LoadedModule,
	in_closure: &HashSet<usize>,
	item_modules: &[usize],
	item_ranges: &[Option<(usize, usize)>],
	used_bindings: &[(usize, ModuleId, NameId)],
) -> Option<String> {
	let items: Vec<usize> = (0..item_modules.len())
		.filter(|idx| item_modules[*idx] == module_idx && in_closure.contains(idx))
		.collect();
	if items.is_empty() {
		return None;
	}
	let used: HashSet<NameId> = used_bindings
		.iter()
		.filter(|(item, binding_module, _)| {
			*binding_module == module.id && in_closure.contains(item)
		})
		.map(|(_, _, name)| *name)
		.collect();

	let mut out = String::new();
	for import in &module.parsed.imports {
		let names: Vec<String> = import
			.names
			.iter()
			.filter(|n| used.contains(&intern_name(&n.local().1)))
			.map(|n| match &n.local {
				Some(local) => format!("{} as {}", n.name.1, local.1),
				None => n.name.1.clone(),
			})
			.collect();
		if names.is_empty() {
			continue;
		}
		let external = if import.external { "@external " } else { "" };
		out.push_str(&format!(
			"import {external}{{ {} }} from \"{}\";\n",
			names.join(", "),
			import.path.1
		));
	}
	for alias in &module.parsed.aliases {
		if used.contains(&intern_name(&alias.name.1)) {
			out.push_str(&format!("alias {} = {};\n", alias.name.1, alias.target.1));
		}
	}
	for idx in items {
		let Some((start, end)) = item_ranges[idx] else {
			continue;
		};
		if !out.is_empty() {
			out.push('\n');
		}
		out.push_str(&module.text[start..end]);
		out.push('\n');
	}
	Some(out)
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use super::{ModuleRole, Project, parse_project};
	use crate::{
		HasIdent, HasUid, diagnostics::Report, parser::SchemaVersion, process::NamingConvention,
		root::SchemaProcessOptions, source::ResolvedSpec, uid::RenameMap,
	};

	fn project(root: &str, files: &[(&str, &str)]) -> Result<Project, Vec<String>> {
		project_roots(&[root], files)
	}

	fn project_roots(roots: &[&str], files: &[(&str, &str)]) -> Result<Project, Vec<String>> {
		let files: BTreeMap<String, String> = files
			.iter()
			.map(|(k, v)| (k.to_string(), v.to_string()))
			.collect();
		let roots: Vec<ResolvedSpec> = roots
			.iter()
			.map(|r| ResolvedSpec::root(r).expect("valid root"))
			.collect();
		let mut report = Report::new();
		let result = parse_project(
			&roots,
			&files,
			SchemaVersion::Current,
			&SchemaProcessOptions {
				generator_supports_domain: true,
				naming_convention: NamingConvention::Postgres,
			},
			&mut RenameMap::new(),
			&mut report,
		);
		match result {
			Ok(p) if !report.is_error() => Ok(p),
			_ => Err(report.parts.into_iter().map(|p| p.msg).collect()),
		}
	}

	fn roles(p: &Project) -> Vec<(&str, ModuleRole)> {
		p.modules.iter().map(|(k, v)| (k.as_str(), *v)).collect()
	}

	const AUTH: &str = r#"
		scalar user_id = sql"INTEGER";
		table User {
			id: user_id @primary_key;
		};
	"#;

	#[test]
	fn import_and_rename() {
		let p = project(
			"db.schema",
			&[
				(
					"db.schema",
					r#"
						import { User, user_id as uid } from "./auth.schema";
						table Post {
							author: uid ~ User(id);
						};
					"#,
				),
				("auth.schema", AUTH),
			],
		)
		.expect("valid");
		let post = p.schema.tables().find(|t| t.id().name() == "Post").unwrap();
		assert_eq!(
			post.columns[0].ty.qualified_name(),
			(Some("auth.schema".to_owned()), "user_id".to_owned())
		);
		assert_eq!(
			post.foreign_keys[0].target.qualified_name(),
			(Some("auth.schema".to_owned()), "User".to_owned())
		);
	}

	#[test]
	fn unimported_name_is_not_visible() {
		let errors = project(
			"db.schema",
			&[
				(
					"db.schema",
					r#"
						import { User } from "./auth.schema";
						table Post {
							author: user_id ~ User(id);
						};
					"#,
				),
				("auth.schema", AUTH),
			],
		)
		.unwrap_err();
		assert_eq!(errors, vec!["unknown type `user_id`"]);
	}

	#[test]
	fn import_errors() {
		let errors = project(
			"db.schema",
			&[(
				"db.schema",
				r#"import { A } from "./missing.schema"; import { B } from "nope.schema";"#,
			)],
		)
		.unwrap_err();
		assert_eq!(
			errors,
			vec![
				"import path \"nope.schema\" should be relative, starting with \"./\" or \"../\", or point to a package, starting with \"@\"",
				"failed to load \"missing.schema\": file \"missing.schema\" not found",
			]
		);
		let errors = project(
			"db.schema",
			&[
				(
					"db.schema",
					r#"
						import { Nope } from "./auth.schema";
						import { User } from "./auth.schema";
						import { user_id as User } from "./auth.schema";
					"#,
				),
				("auth.schema", AUTH),
			],
		)
		.unwrap_err();
		assert_eq!(
			errors,
			vec![
				"`User` is already defined in this module",
				"module \"auth.schema\" does not declare `Nope`",
			]
		);
	}

	#[test]
	fn private_items_with_same_name() {
		let p = project(
			"db.schema",
			&[
				(
					"db.schema",
					r#"
						import { Audit as OtherAudit } from "./other.schema";
						scalar id = sql"INTEGER";
						table Audit "audit_main" {
							id;
						};
					"#,
				),
				(
					"other.schema",
					r#"
						scalar id "other_id" = sql"TEXT";
						table Audit "audit_other" {
							id;
						};
					"#,
				),
			],
		)
		.expect("valid");
		assert_eq!(p.schema.tables().count(), 2);
		assert_eq!(p.schema.scalars().count(), 2);
	}

	#[test]
	fn private_items_with_same_db_name() {
		let errors = project(
			"db.schema",
			&[
				(
					"db.schema",
					r#"
						import { Audit as OtherAudit } from "./other.schema";
						scalar x "main_x" = sql"INTEGER";
						table Audit { x; };
					"#,
				),
				(
					"other.schema",
					r#"scalar x "other_x" = sql"INTEGER"; table Audit { x; };"#,
				),
			],
		)
		.unwrap_err();
		assert_eq!(
			errors,
			vec![
				"database name `audits` is used by multiple items, consider setting an explicit database name"
			]
		);
	}

	#[test]
	fn mixin_is_lexically_scoped() {
		let p = project(
			"db.schema",
			&[
				(
					"db.schema",
					r#"
						import { Tracked } from "./mixins.schema";
						scalar id = sql"INTEGER";
						table T {
							@mixin Tracked;
							id;
						};
					"#,
				),
				(
					"mixins.schema",
					r#"
						scalar stamp = sql"TIMESTAMP";
						@mixin Tracked {
							created: stamp;
						};
					"#,
				),
			],
		)
		.expect("valid");
		let t = p.schema.tables().next().unwrap();
		let types: Vec<_> = t.columns.iter().map(|c| c.ty.qualified_name()).collect();
		assert_eq!(
			types,
			vec![
				(Some("db.schema".to_owned()), "id".to_owned()),
				(Some("mixins.schema".to_owned()), "stamp".to_owned()),
			]
		);
	}

	const A: &str = r#"
		import { b_id } from "./b.schema";
		scalar a_id = sql"INTEGER";
		table A { a_id; b_id; };
	"#;

	#[test]
	fn managed_wins_over_external() {
		let b = r#"
			import @external { a_id } from "./a.schema";
			scalar b_id = sql"INTEGER";
			table B { a_id; b_id; };
		"#;
		let files = [("a.schema", A), ("b.schema", b)];
		let p = project("a.schema", &files).expect("valid");
		assert_eq!(
			roles(&p),
			vec![
				("a.schema", ModuleRole::Managed),
				("b.schema", ModuleRole::Managed)
			]
		);
		let p = project("b.schema", &files).expect("valid");
		assert_eq!(
			roles(&p),
			vec![
				("a.schema", ModuleRole::External),
				("b.schema", ModuleRole::Managed)
			]
		);
	}

	#[test]
	fn external_slices() {
		let files = [
			(
				"db.schema",
				"import @external { Invoice, invoice_id as iid } from \"../billing/db.schema\";\ntable Payment {\n\tinvoice: iid ~ Invoice(id);\n};\n",
			),
			(
				"../billing/db.schema",
				"import { customer_id, Customer } from \"./customers.schema\";\nimport { Unused } from \"./unused.schema\";\nalias cid = customer_id;\nalias other = Unused;\nscalar invoice_id = sql\"INTEGER\";\n// docs\ntable Invoice {\n\tid: invoice_id @primary_key;\n\tcustomer: cid ~ Customer(id);\n\t@index(customer);\n};\ntable Unrelated {\n\tid: invoice_id ~ Unused;\n};\n",
			),
			(
				"../billing/customers.schema",
				"scalar customer_id = sql\"INTEGER\";\ntable Customer {\n\tid: customer_id @primary_key;\n};\n",
			),
			(
				"../billing/unused.schema",
				"scalar x = sql\"INTEGER\";\ntable Unused {\n\tx @primary_key;\n};\n",
			),
		];
		let p = project("db.schema", &files).expect("valid");
		let sources: Vec<(&str, &str)> = p
			.sources
			.iter()
			.map(|(k, v)| (k.as_str(), v.as_str()))
			.collect();
		assert_eq!(
			sources,
			vec![
				(
					"../billing/customers.schema",
					"scalar customer_id = sql\"INTEGER\";\n"
				),
				(
					"../billing/db.schema",
					"import { customer_id } from \"./customers.schema\";\nalias cid = customer_id;\n\nscalar invoice_id = sql\"INTEGER\";\n\n// docs\ntable Invoice {\n\tid: invoice_id @primary_key;\n\tcustomer: cid ~ Customer(id);\n\t@index(customer);\n};\n"
				),
				("db.schema", files[0].1),
			]
		);
		let mut stubs: Vec<String> = p
			.schema
			.0
			.iter()
			.filter(|i| p.schema.is_stub(i.uid()))
			.map(|i| super::item_ident(i).name())
			.collect();
		stubs.sort();
		assert_eq!(stubs, vec!["Invoice", "customer_id", "invoice_id"]);

		let snapshot: Vec<(&str, &str)> = p
			.sources
			.iter()
			.map(|(k, v)| (k.as_str(), v.as_str()))
			.collect();
		let again = project("db.schema", &snapshot).expect("snapshot is self-contained");
		assert_eq!(again.sources, p.sources);
	}

	#[test]
	fn plugin_roots() {
		let files = [
			(
				"@main/db.schema",
				"import { Page } from \"@logseq/db.schema\";\nscalar user_id = sql\"INTEGER\";\ntable User {\n\tid: user_id @primary_key;\n\thome: page_id ~ Page(id);\n};\nimport { page_id } from \"@logseq/types/ids.schema\";\n",
			),
			(
				"@logseq/db.schema",
				"import { User, user_id } from \"@main/db.schema\";\nimport { page_id } from \"./types/ids.schema\";\ntable Page {\n\tid: page_id @primary_key;\n\towner: user_id ~ User(id);\n};\n",
			),
			(
				"@logseq/types/ids.schema",
				"scalar page_id = sql\"INTEGER\";\n",
			),
		];
		let p = project_roots(&["@main/db.schema", "@logseq/db.schema"], &files).expect("valid");
		assert_eq!(
			roles(&p),
			vec![
				("@logseq/db.schema", ModuleRole::Managed),
				("@logseq/types/ids.schema", ModuleRole::Managed),
				("@main/db.schema", ModuleRole::Managed),
			]
		);
		assert_eq!(p.schema.tables().count(), 2);
	}

	#[test]
	fn side_effect_import() {
		let p = project(
			"db.schema",
			&[
				("db.schema", "import \"@plugin/db.schema\";\n"),
				(
					"@plugin/db.schema",
					"scalar x = sql\"INTEGER\";\ntable T { x; };\n",
				),
			],
		)
		.expect("valid");
		assert_eq!(p.schema.tables().count(), 1);
		let errors = project(
			"db.schema",
			&[
				(
					"db.schema",
					"import \"@plugin/db.schema\";\nscalar y = sql\"INTEGER\";\ntable U { x; };\n",
				),
				("@plugin/db.schema", "scalar x = sql\"INTEGER\";\n"),
			],
		)
		.unwrap_err();
		assert_eq!(errors, vec!["unknown type `x`"]);
	}

	#[test]
	fn package_escape() {
		let errors = project(
			"@a/db.schema",
			&[("@a/db.schema", "import \"../b/db.schema\";\n")],
		)
		.unwrap_err();
		assert_eq!(
			errors,
			vec!["path \"../b/db.schema\" points outside of the package"]
		);
	}

	#[test]
	fn import_cycle() {
		let b = r#"
			import { a_id } from "./a.schema";
			scalar b_id = sql"INTEGER";
			table B { a_id; b_id; };
		"#;
		let p = project("a.schema", &[("a.schema", A), ("b.schema", b)]).expect("valid");
		assert_eq!(p.schema.tables().count(), 2);
	}
}
