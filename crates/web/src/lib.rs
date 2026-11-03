use std::collections::BTreeMap;

use file_diffs::{Migration as FileMigration, Snapshot as FileSnapshot};
use generator_postgres::Pg;
use generator_postgres::migration::{NonDestructive, UserSql, migration_sql};
use js_sys::{Array, Function, Object, Reflect};
use schema::diagnostics::Report;
use schema::parser::{LATEST_SCHEMA_VERSION, ROOT_MODULE, SchemaVersion};
use schema::project::parse_project;
use schema::root::{Schema, SchemaProcessOptions};
use schema::source::{ResolvedSpec, SourceLoader};
use schema::uid::RenameMap;
use wasm_bindgen::prelude::{JsError, JsValue, wasm_bindgen};

#[wasm_bindgen]
#[derive(Clone, Copy)]
pub enum ReportSeverity {
	Warn,
	Error,
}

#[derive(Clone)]
#[wasm_bindgen(getter_with_clone)]
pub struct ReportLine {
	pub file: String,
	pub from: u32,
	pub to: u32,
	pub description: String,
	pub severity: ReportSeverity,
}

#[wasm_bindgen(getter_with_clone)]
pub struct DiffResult {
	pub errors_a: Vec<ReportLine>,
	pub errors_b: Vec<ReportLine>,
	pub diff_up: Option<String>,
	pub diff_down: Option<String>,
}

fn process_report(report: Report) -> Vec<ReportLine> {
	report
		.parts
		.into_iter()
		.map(|p| {
			let (file, from, to) = match p.annotations.first() {
				Some(a) => (a.span.source().path(), a.span.start, a.span.end),
				None => (String::new(), 0, 0),
			};
			ReportLine {
				file,
				from,
				to,
				description: p.msg,
				severity: match p.severity {
					schema::diagnostics::Severity::Warning => ReportSeverity::Warn,
					schema::diagnostics::Severity::Error => ReportSeverity::Error,
				},
			}
		})
		.collect()
}

fn parse(
	roots: &[String],
	files: &BTreeMap<String, String>,
	rn: &mut RenameMap,
	report: &mut Report,
) -> Option<Schema> {
	let mut resolved = Vec::new();
	for root in roots {
		match ResolvedSpec::root(root) {
			Ok(root) => resolved.push(root),
			Err(e) => {
				report.error(format!("invalid root: {e}"));
				return None;
			}
		}
	}
	parse_project(
		&resolved,
		files,
		SchemaVersion::Current,
		&SchemaProcessOptions {
			generator_supports_domain: true,
			naming_convention: schema::process::NamingConvention::Postgres,
		},
		rn,
		report,
	)
	.ok()
	.map(|p| p.schema)
}

fn diff_files(
	a_roots: &[String],
	a: BTreeMap<String, String>,
	b_roots: &[String],
	b: BTreeMap<String, String>,
) -> DiffResult {
	console_error_panic_hook::set_once();
	let mut report_a = Report::new();
	let mut report_b = Report::new();
	let mut rn = RenameMap::new();
	let a = parse(a_roots, &a, &mut rn, &mut report_a);
	let b = parse(b_roots, &b, &mut rn, &mut report_b);

	let (diff_up, diff_down) = match (a, b) {
		(Some(a), Some(b)) => {
			let mut diff_up = String::new();
			let mut diff_down = String::new();
			Pg(&b).diff(
				&Pg(&a),
				&mut diff_up,
				&mut rn.clone(),
				&mut report_a,
				&mut report_b,
			);
			Pg(&a).diff(
				&Pg(&b),
				&mut diff_down,
				&mut rn.clone(),
				&mut report_b,
				&mut report_a,
			);
			(Some(diff_up), Some(diff_down))
		}
		_ => (None, None),
	};

	DiffResult {
		errors_a: process_report(report_a),
		errors_b: process_report(report_b),
		diff_up,
		diff_down,
	}
}

fn object_files(files: &Object) -> BTreeMap<String, String> {
	Object::entries(files)
		.iter()
		.filter_map(|entry| {
			let entry = Array::from(&entry);
			Some((entry.get(0).as_string()?, entry.get(1).as_string()?))
		})
		.collect()
}

#[wasm_bindgen]
pub fn diff(a: String, b: String) -> DiffResult {
	let roots = [ROOT_MODULE.to_owned()];
	diff_files(
		&roots,
		BTreeMap::from([(ROOT_MODULE.to_owned(), a)]),
		&roots,
		BTreeMap::from([(ROOT_MODULE.to_owned(), b)]),
	)
}

#[wasm_bindgen]
pub fn diff_project(
	old_roots: Vec<String>,
	old_files: Object,
	new_roots: Vec<String>,
	new_files: Object,
) -> DiffResult {
	diff_files(
		&old_roots,
		object_files(&old_files),
		&new_roots,
		object_files(&new_files),
	)
}

fn files_object(files: &BTreeMap<String, String>) -> Object {
	let object = Object::new();
	for (path, text) in files {
		Reflect::set(&object, &JsValue::from_str(path), &JsValue::from_str(text))
			.expect("plain object accepts string keys");
	}
	object
}

fn options() -> SchemaProcessOptions {
	SchemaProcessOptions {
		generator_supports_domain: true,
		naming_convention: schema::process::NamingConvention::Postgres,
	}
}

fn render(report: &Report) -> String {
	let mut out = String::new();
	for msg in report.plain_errors() {
		out.push_str(&format!("error: {msg}\n"));
	}
	for source in report.clone().to_hi_doc() {
		out.push_str(&hi_doc::source_to_ansi(&source));
	}
	out
}

fn resolve_roots(roots: &[String]) -> Result<Vec<ResolvedSpec>, String> {
	roots
		.iter()
		.map(|r| ResolvedSpec::root(r).map_err(|e| format!("invalid root: {e}")))
		.collect()
}

#[wasm_bindgen]
#[derive(Clone)]
pub struct Snapshot(FileSnapshot);

#[wasm_bindgen]
impl Snapshot {
	#[wasm_bindgen(constructor)]
	pub fn new(roots: Vec<String>, files: Object) -> Snapshot {
		Snapshot(FileSnapshot {
			roots,
			files: object_files(&files),
		})
	}
	pub fn empty() -> Snapshot {
		Snapshot(FileSnapshot::default())
	}
	#[wasm_bindgen(getter)]
	pub fn roots(&self) -> Vec<String> {
		self.0.roots.clone()
	}
	#[wasm_bindgen(getter)]
	pub fn files(&self) -> Object {
		files_object(&self.0.files)
	}
}

#[wasm_bindgen]
pub struct Migration(FileMigration);

#[wasm_bindgen]
impl Migration {
	pub fn parse(text: &str) -> Result<Migration, JsError> {
		text.parse::<FileMigration>()
			.map(Migration)
			.map_err(|e| JsError::new(&e.to_string()))
	}
	#[allow(clippy::too_many_arguments)]
	pub fn create(
		name: String,
		description: String,
		snapshot: &Snapshot,
		before_up_sql: Option<String>,
		after_up_sql: Option<String>,
		before_down_sql: Option<String>,
		after_down_sql: Option<String>,
	) -> Migration {
		Migration(FileMigration::new(
			name,
			description,
			LATEST_SCHEMA_VERSION,
			before_up_sql,
			after_up_sql,
			before_down_sql,
			after_down_sql,
			snapshot.0.clone(),
		))
	}
	#[wasm_bindgen(js_name = toString)]
	pub fn to_text(&self) -> String {
		self.0.to_string()
	}
	#[wasm_bindgen(js_name = checkString)]
	pub fn check_string(&self) -> String {
		self.0.schema_check_string()
	}
	#[wasm_bindgen(js_name = isNoop)]
	pub fn is_noop(&self) -> bool {
		self.0.is_noop()
	}
	#[wasm_bindgen(getter, js_name = schemaVersion)]
	pub fn schema_version(&self) -> u32 {
		self.0.schema_version
	}
	#[wasm_bindgen(getter, js_name = keepObsolete)]
	pub fn keep_obsolete(&self) -> Option<String> {
		self.0.keep_obsolete.clone()
	}
	#[wasm_bindgen(setter, js_name = keepObsolete)]
	pub fn set_keep_obsolete(&mut self, keep: Option<String>) {
		self.0.keep_obsolete = keep;
	}
	#[wasm_bindgen(js_name = toDiff)]
	pub fn to_diff(&mut self, old: &Snapshot) -> Result<(), JsError> {
		self.0
			.to_diff(&old.0)
			.map_err(|e| JsError::new(&e.to_string()))
	}
	pub fn apply(&self, old: &Snapshot) -> Result<Snapshot, JsError> {
		self.0
			.apply_diff(old.0.clone())
			.map(Snapshot)
			.map_err(|e| JsError::new(&e.to_string()))
	}
}

#[wasm_bindgen(js_name = migrationSlug)]
pub fn migration_slug(name: &str) -> String {
	slug::slugify(name)
}

struct JsLoader<'f>(&'f Function);
impl SourceLoader for JsLoader<'_> {
	fn load(&self, path: &ResolvedSpec) -> Result<String, String> {
		let value = self
			.0
			.call1(&JsValue::NULL, &JsValue::from_str(path.as_str()))
			.map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))?;
		value
			.as_string()
			.ok_or_else(|| format!("file {:?} not found", path.as_str()))
	}
}

#[wasm_bindgen(js_name = loadSchema)]
pub fn load_schema(roots: Vec<String>, load: &Function) -> Result<Snapshot, JsError> {
	console_error_panic_hook::set_once();
	let resolved = resolve_roots(&roots).map_err(|e| JsError::new(&e))?;
	let mut report = Report::new();
	let mut rn = RenameMap::new();
	let project = parse_project(
		&resolved,
		&JsLoader(load),
		SchemaVersion::Current,
		&options(),
		&mut rn,
		&mut report,
	)
	.map_err(|()| JsError::new(&render(&report)))?;
	generator_postgres::validate::validate(&project.schema, &rn);
	Ok(Snapshot(FileSnapshot {
		roots,
		files: project.sources,
	}))
}

fn parse_snapshot(
	snapshot: &FileSnapshot,
	version: SchemaVersion,
	rn: &mut RenameMap,
	report: &mut Report,
) -> Option<Schema> {
	let roots = match resolve_roots(&snapshot.roots) {
		Ok(roots) => roots,
		Err(e) => {
			report.error(e);
			return None;
		}
	};
	let mut files = snapshot.files.clone();
	for root in &snapshot.roots {
		files.entry(root.clone()).or_default();
	}
	parse_project(&roots, &files, version, &options(), rn, report)
		.ok()
		.map(|p| p.schema)
}

#[wasm_bindgen(getter_with_clone)]
pub struct SqlResult {
	pub up: String,
	pub down: String,
	pub failed: bool,
	pub errors: String,
}

fn sql_between(
	old: &Snapshot,
	old_version: u32,
	new: &Snapshot,
	sql: impl FnOnce(&Schema, &Schema, &RenameMap, &mut Report, &mut Report) -> (String, String),
) -> SqlResult {
	console_error_panic_hook::set_once();
	let mut rn = RenameMap::new();
	let mut report_old = Report::new();
	let mut report_new = Report::new();
	let old = parse_snapshot(
		&old.0,
		SchemaVersion::Stored(old_version),
		&mut rn,
		&mut report_old,
	);
	let new = parse_snapshot(&new.0, SchemaVersion::Current, &mut rn, &mut report_new);
	let (up, down) = match (&old, &new) {
		(Some(old), Some(new)) => sql(old, new, &rn, &mut report_old, &mut report_new),
		_ => (String::new(), String::new()),
	};
	SqlResult {
		up,
		down,
		failed: old.is_none() || new.is_none() || report_old.is_error() || report_new.is_error(),
		errors: format!("{}{}", render(&report_old), render(&report_new)),
	}
}

#[wasm_bindgen(js_name = generateSql)]
pub fn generate_sql(
	migration: &Migration,
	old: &Snapshot,
	old_version: u32,
	new: &Snapshot,
	fail_on_data_loss: bool,
) -> SqlResult {
	let user = UserSql {
		before_up: migration.0.before_up_sql.clone(),
		after_up: migration.0.after_up_sql.clone(),
		before_down: migration.0.before_down_sql.clone(),
		after_down: migration.0.after_down_sql.clone(),
	};
	let non_destructive = match &migration.0.keep_obsolete {
		Some(keep) => NonDestructive::Keep(keep.clone()),
		None if fail_on_data_loss => NonDestructive::Fail,
		None => NonDestructive::Off,
	};
	sql_between(old, old_version, new, |old, new, rn, ro, rnw| {
		migration_sql(&user, &non_destructive, old, new, rn, ro, rnw)
	})
}

#[wasm_bindgen(js_name = diffSql)]
pub fn diff_sql(old: &Snapshot, old_version: u32, new: &Snapshot) -> SqlResult {
	sql_between(old, old_version, new, |old, new, rn, ro, rnw| {
		let mut up = String::new();
		let mut down = String::new();
		Pg(new).diff(&Pg(old), &mut up, &mut rn.clone(), ro, rnw);
		Pg(old).diff(&Pg(new), &mut down, &mut rn.clone(), rnw, ro);
		(up, down)
	})
}

#[test]
fn test() {
	diff(
		r#"
scalar int = sql"INTEGER" @check(t);

table Example {
	a: int;
	b: int;
};
		"#
		.to_owned(),
		r#"
scalar int = sql"INTEGER" @check(t);
scalar int
"#
		.to_owned(),
	);
}

#[test]
fn test_files() {
	let plugins = vec!["@main/db.schema".to_owned(), "@extra/db.schema".to_owned()];
	let files = BTreeMap::from([
		(
			"@main/db.schema".to_owned(),
			"import { id } from \"./types.schema\";\ntable Alpha { id; };\n".to_owned(),
		),
		(
			"@main/types.schema".to_owned(),
			"scalar id = sql\"INTEGER\";\n".to_owned(),
		),
		(
			"@extra/db.schema".to_owned(),
			"import { id } from \"@main/types.schema\";\ntable Beta { id; };\n".to_owned(),
		),
	]);
	let result = diff_files(&[], BTreeMap::new(), &plugins, files.clone());
	assert!(result.errors_a.is_empty());
	assert!(result.errors_b.is_empty());
	let up = result.diff_up.expect("diff");
	assert!(up.contains("CREATE TABLE alphas"));
	assert!(up.contains("CREATE TABLE betas"));

	let result = diff_files(&plugins, files.clone(), &plugins[..1], files);
	assert_eq!(result.diff_up.expect("diff").trim(), "DROP TABLE betas;");

	let roots = [ROOT_MODULE.to_owned()];
	let result = diff_files(
		&roots,
		BTreeMap::from([(ROOT_MODULE.to_owned(), String::new())]),
		&roots,
		BTreeMap::from([(
			ROOT_MODULE.to_owned(),
			"import { id } from \"./missing.schema\";\n".to_owned(),
		)]),
	);
	assert_eq!(result.errors_b[0].file, "db.schema");
}
