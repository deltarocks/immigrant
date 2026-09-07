use std::{env::current_dir, fs, path::PathBuf, process};

use clap::Parser;
use cli::current_schema;
use file_diffs::find_root;
use schema::annotation::Tracker;

mod generate;
mod yaml;

use generate::{
	Options, TRACKED_ANNOTATIONS, build, model_sql, project_yaml, sanitize_name, schema_yaml,
	sources_yaml,
};

/// immigrant dbt project generation
#[derive(Parser)]
#[clap(author, version)]
enum Opts {
	/// Generate a dbt project from db.schema: one model per table, sources.yml,
	/// schema.yml with tests and lightdash metadata.
	///
	/// dbt_project.yml is written only when missing. Tables and columns with
	/// #dbt(skip) are omitted, #dbt(label = "...") sets the display label.
	Generate {
		/// Output directory
		#[clap(long, default_value = "dbt")]
		out: PathBuf,
		/// dbt project name, defaults to the directory containing migrations
		#[clap(long)]
		project: Option<String>,
		/// dbt source name, defaults to the project name
		#[clap(long)]
		source: Option<String>,
		/// Database schema holding the tables
		#[clap(long, default_value = "public")]
		schema: String,
	},
}

fn main() -> anyhow::Result<()> {
	match Opts::parse() {
		Opts::Generate {
			out,
			project,
			source,
			schema,
		} => generate(out, project, source, schema),
	}
}

fn generate(
	out: PathBuf,
	project: Option<String>,
	source: Option<String>,
	db_schema: String,
) -> anyhow::Result<()> {
	let root = find_root(&current_dir()?)?;
	let project = project.unwrap_or_else(|| {
		root.parent()
			.and_then(|p| p.file_name())
			.map(|n| n.to_string_lossy().into_owned())
			.unwrap_or_else(|| "immigrant".to_owned())
	});
	let project = sanitize_name(&project);
	let opts = Options {
		source: source.unwrap_or_else(|| project.clone()),
		project,
		schema: db_schema,
	};

	let (src, schema, mut report, rn) = current_schema(&root)?;
	let mut tracker = Tracker::new(TRACKED_ANNOTATIONS);
	let models = build(&schema, &rn, &mut report, &opts, &mut tracker);
	tracker.report_unused(&mut report);
	if report.is_error() {
		eprintln!("schema parsing failed:");
		for s in report.to_hi_doc(&src) {
			eprint!("{}", hi_doc::source_to_ansi(&s));
		}
		process::exit(1);
	}

	let models_dir = out.join("models");
	fs::create_dir_all(&models_dir)?;
	fs::write(models_dir.join("sources.yml"), sources_yaml(&models, &opts))?;
	fs::write(models_dir.join("schema.yml"), schema_yaml(&models))?;
	for model in &models {
		fs::write(
			models_dir.join(format!("{}.sql", model.name)),
			model_sql(model, &opts),
		)?;
	}
	let project_file = out.join("dbt_project.yml");
	if !project_file.exists() {
		fs::write(&project_file, project_yaml(&opts))?;
	}
	eprintln!(
		"generated {} models into {}",
		models.len(),
		models_dir.display()
	);
	Ok(())
}

#[cfg(test)]
mod tests {
	use schema::{
		annotation::Tracker,
		diagnostics::Report,
		parser::{SchemaVersion, parse},
		process::NamingConvention,
		root::SchemaProcessOptions,
		uid::RenameMap,
	};

	use crate::generate::{
		Options, TRACKED_ANNOTATIONS, build, model_sql, schema_yaml, sources_yaml,
	};

	fn run(src: &str) -> Result<(String, String, Vec<String>), String> {
		let mut rn = RenameMap::default();
		let mut report = Report::new();
		let schema = parse(
			src,
			SchemaVersion::Current,
			&SchemaProcessOptions {
				generator_supports_domain: true,
				naming_convention: NamingConvention::Postgres,
			},
			&mut rn,
			&mut report,
		)
		.expect("parse result");
		assert!(!report.is_error());
		let opts = Options {
			project: "test".to_owned(),
			source: "test".to_owned(),
			schema: "public".to_owned(),
		};
		let mut tracker = Tracker::new(TRACKED_ANNOTATIONS);
		let models = build(&schema, &rn, &mut report, &opts, &mut tracker);
		tracker.report_unused(&mut report);
		if report.is_error() {
			return Err(report
				.to_hi_doc(src)
				.iter()
				.map(hi_doc::source_to_ansi)
				.collect::<Vec<_>>()
				.join("\n"));
		}
		let sql = models.iter().map(|m| model_sql(m, &opts)).collect();
		Ok((schema_yaml(&models), sources_yaml(&models, &opts), sql))
	}
	fn generate(src: &str) -> (String, String, Vec<String>) {
		run(src).expect("generate")
	}

	fn strip_ansi(s: &str) -> String {
		let mut out = String::new();
		let mut chars = s.chars();
		while let Some(c) = chars.next() {
			if c != '\x1b' {
				out.push(c);
				continue;
			}
			for c in chars.by_ref() {
				if c.is_ascii_alphabetic() {
					break;
				}
			}
		}
		out.lines()
			.map(str::trim_end)
			.collect::<Vec<_>>()
			.join("\n")
	}

	#[test]
	fn basic() {
		let (schema, sources, sql) = generate(include_str!("../tests/basic.schema"));
		insta::assert_snapshot!("basic_schema", schema);
		insta::assert_snapshot!("basic_sources", sources);
		insta::assert_snapshot!("basic_sql", sql.join("\n"));
	}

	#[test]
	fn unknown_pragma_field() {
		let err = run(include_str!("../tests/unknown_field.schema")).unwrap_err();
		insta::assert_snapshot!(strip_ansi(&err));
	}
}
