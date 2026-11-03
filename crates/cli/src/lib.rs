use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
	str::FromStr,
};

use anyhow::{anyhow, bail};
use file_diffs::{Migration, MigrationId, Snapshot};
use generator_postgres::migration::{NonDestructive, UserSql, migration_sql};
use schema::{
	HasIdent,
	diagnostics::Report,
	parser::SchemaVersion,
	process::NamingConvention,
	project::{Project, parse_project},
	root::{Schema, SchemaProcessOptions},
	source::{ResolvedSpec, SourceLoader},
	uid::RenameMap,
};
use tracing::debug;

pub struct FsLoader {
	pub root: PathBuf,
}
impl SourceLoader for FsLoader {
	fn load(&self, path: &ResolvedSpec) -> Result<String, String> {
		if path.package().is_some() {
			return Err("package imports are not supported for schemas stored on disk".to_owned());
		}
		fs::read_to_string(self.root.join(path.as_str())).map_err(|e| e.to_string())
	}
}

fn print_report(report: Report) {
	for msg in report.plain_errors() {
		eprintln!("error: {msg}");
	}
	for s in report.to_hi_doc() {
		eprint!("{}", hi_doc::source_to_ansi(&s));
	}
}

pub fn parse_schema(
	loader: &dyn SourceLoader,
	roots: &[String],
	version: SchemaVersion,
	rn: &mut RenameMap,
) -> anyhow::Result<(Project, Report)> {
	let mut report = Report::new();
	let roots = roots
		.iter()
		.map(|r| ResolvedSpec::root(r).map_err(|e| anyhow!("invalid root: {e}")))
		.collect::<anyhow::Result<Vec<_>>>()?;
	let project = parse_project(
		&roots,
		loader,
		version,
		&SchemaProcessOptions {
			generator_supports_domain: true,
			naming_convention: NamingConvention::Postgres,
		},
		rn,
		&mut report,
	);
	match project {
		Ok(project) => Ok((project, report)),
		Err(()) => {
			eprintln!("schema parsing ended with failure:");
			print_report(report);
			Err(anyhow!("failed to parse schema"))
		}
	}
}

pub fn current_schema(dir: &Path) -> anyhow::Result<(Snapshot, Schema, Report, RenameMap)> {
	let mut rn = RenameMap::default();
	let loader = FsLoader {
		root: dir.to_path_buf(),
	};
	let roots = Snapshot::default().roots;
	let (project, report) = parse_schema(&loader, &roots, SchemaVersion::Current, &mut rn)?;
	generator_postgres::validate::validate(&project.schema, &rn);
	let snapshot = Snapshot {
		roots,
		files: project.sources,
	};
	Ok((snapshot, project.schema, report, rn))
}

pub fn check_unique_code_names(schema: &Schema) -> anyhow::Result<()> {
	let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
	for item in schema.material_items() {
		let (module, name) = item.id().qualified_name();
		seen.entry(name)
			.or_default()
			.push(module.unwrap_or_default());
	}
	let duplicates: Vec<String> = seen
		.into_iter()
		.filter(|(_, modules)| modules.len() > 1)
		.map(|(name, modules)| format!("`{name}` in {}", modules.join(", ")))
		.collect();
	if !duplicates.is_empty() {
		bail!(
			"code generation requires unique item names across files, consider renaming: {}",
			duplicates.join("; ")
		);
	}
	Ok(())
}

pub fn parse_stored(
	snapshot: &Snapshot,
	version: u32,
	rn: &mut RenameMap,
) -> anyhow::Result<(Schema, Report)> {
	let mut loader = snapshot.files.clone();
	for root in &snapshot.roots {
		loader.entry(root.clone()).or_default();
	}
	let (project, report) =
		parse_schema(&loader, &snapshot.roots, SchemaVersion::Stored(version), rn)?;
	Ok((project.schema, report))
}

pub fn stored_schema(
	list: &[(MigrationId, Migration, PathBuf)],
) -> anyhow::Result<(Snapshot, Schema, Report, RenameMap)> {
	let mut sources = Snapshot::default();
	debug!("applying patches");
	for (i, migration, _) in list {
		debug!("patch {}", i.slug);
		sources = migration.apply_diff(sources)?;
	}
	let version = list
		.iter()
		.map(|(_, m, _)| m.schema_version)
		.max()
		.unwrap_or(1);
	let mut rn = RenameMap::default();
	let (schema, report) = parse_stored(&sources, version, &mut rn)?;
	Ok((sources, schema, report, rn))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NonDestructiveMode {
	Keep,
	KeepAs(String),
	Fail,
}
impl FromStr for NonDestructiveMode {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"keep" => Ok(Self::Keep),
			"fail" => Ok(Self::Fail),
			id if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
				Ok(Self::KeepAs(id.to_owned()))
			}
			_ => Err(format!(
				"expected `keep`, `fail` or keep id consisting of ascii letters, digits and underscores, got {s:?}"
			)),
		}
	}
}

pub fn configure_non_destructive(
	migration: &mut Migration,
	mode: Option<&NonDestructiveMode>,
	id: u32,
) -> bool {
	match mode {
		None => false,
		Some(NonDestructiveMode::Fail) => true,
		Some(NonDestructiveMode::Keep) => {
			migration.keep_obsolete = Some(id.to_string());
			false
		}
		Some(NonDestructiveMode::KeepAs(keep)) => {
			migration.keep_obsolete = Some(keep.clone());
			false
		}
	}
}

pub fn generate_sql_nowrite(
	migration: &Migration,
	fail_on_data_loss: bool,
	old_schema: &Schema,
	new_schema: &Schema,
	rn: &RenameMap,
	report_old: &mut Report,
	report_new: &mut Report,
) -> anyhow::Result<(String, String)> {
	let user = UserSql {
		before_up: migration.before_up_sql.clone(),
		after_up: migration.after_up_sql.clone(),
		before_down: migration.before_down_sql.clone(),
		after_down: migration.after_down_sql.clone(),
	};
	let non_destructive = match &migration.keep_obsolete {
		Some(keep) => NonDestructive::Keep(keep.clone()),
		None if fail_on_data_loss => NonDestructive::Fail,
		None => NonDestructive::Off,
	};
	Ok(migration_sql(
		&user,
		&non_destructive,
		old_schema,
		new_schema,
		rn,
		report_old,
		report_new,
	))
}
#[allow(clippy::too_many_arguments)]
pub fn generate_sql(
	migration: &Migration,
	fail_on_data_loss: bool,
	old_schema: &Schema,
	new_schema: &Schema,
	rn: &RenameMap,
	out: &Path,
	mut report_old: Report,
	mut report_new: Report,
) -> anyhow::Result<()> {
	let (up, down) = generate_sql_nowrite(
		migration,
		fail_on_data_loss,
		old_schema,
		new_schema,
		rn,
		&mut report_old,
		&mut report_new,
	)?;

	if display_reports(report_old, report_new) {
		bail!("errors are reported, cannot continue");
	}

	let mut out = out.to_path_buf();
	out.push("up.sql");
	fs::write(&out, up.as_bytes())?;
	out.pop();
	out.push("down.sql");
	fs::write(&out, down.as_bytes())?;
	Ok(())
}

pub fn display_reports(ar: Report, br: Report) -> bool {
	if ar.is_error() || br.is_error() {
		eprintln!("schema diffing ended with failure:");
		print_report(ar);
		print_report(br);
		true
	} else {
		false
	}
}
