use std::{
	collections::{BTreeMap, BTreeSet},
	env::{self, current_dir, current_exe},
	ffi::OsString,
	fs::{self, read_dir},
	io::ErrorKind,
	path::{Path, PathBuf},
	process,
};

use anyhow::{Context, bail};
use clap::{CommandFactory, FromArgMatches, Parser};
use cli::{current_schema, display_reports, generate_sql, parse_schema, stored_schema};
use file_diffs::{Migration, MigrationId, find_root, list, list_ids};
use generator_postgres::Pg;
use schema::{diagnostics::Report, root::Schema, uid::RenameMap};

#[derive(Parser)]
#[clap(author, version, allow_external_subcommands = true)]
enum Opts {
	/// Create migrations directory and empty schema.
	Init,
	/// Commit schema changes.
	Commit {
		/// How to name the change.
		///
		/// If not set - commit editor will be opened.
		#[clap(long, short = 'm')]
		message: Option<String>,
		#[clap(long)]
		before_up_sql: Option<String>,
		#[clap(long)]
		after_up_sql: Option<String>,
		#[clap(long)]
		before_down_sql: Option<String>,
		#[clap(long)]
		after_down_sql: Option<String>,
	},
	/// Show the difference between stored and uncommited schema.
	Diff,
	/// Regenerate up.sql/down.sql files.
	///
	/// Run it in case if you have altered the schema file for before/after sql injection.
	RegenerateSql,
	/// List migrations
	List,
}

fn external_cmds_from_dir(dir: &Path, prefix: &str, out: &mut BTreeMap<String, PathBuf>) {
	let Ok(read) = read_dir(dir) else {
		return;
	};
	for entry in read {
		let Ok(entry) = entry else {
			continue;
		};
		let file_name = entry.file_name();
		let Some(name) = file_name.to_str() else {
			continue;
		};
		let Some(cmd) = name.strip_prefix(prefix) else {
			continue;
		};
		if cmd.ends_with(".d") {
			// Exists in cargo output directory
			continue;
		}
		if !entry.metadata().map(|e| e.is_file()).unwrap_or(false) {
			continue;
		}
		// Prefer first found
		if out.contains_key(cmd) {
			continue;
		}
		out.insert(cmd.to_owned(), entry.path());
	}
}

fn external_cmds_from_env(env: &str, prefix: &str, out: &mut BTreeMap<String, PathBuf>) {
	let path = env::var_os(env).unwrap_or_default();
	for ele in env::split_paths(&path) {
		external_cmds_from_dir(&ele, prefix, out)
	}
}

fn parse_or_external<T: CommandFactory + FromArgMatches>() -> T {
	let mut external = BTreeMap::new();
	if let Ok(mut exe) = current_exe() {
		exe.pop();
		external_cmds_from_dir(&exe, "immigrant-", &mut external);
	}
	external_cmds_from_env("PATH", "immigrant-", &mut external);

	let mut cmd = T::command().allow_external_subcommands(true);
	let mut disabled_external = BTreeSet::new();
	for name in external.keys() {
		if cmd.find_subcommand(name).is_some() {
			eprintln!("external subcommand has conflict: {name}");
			disabled_external.insert(name.clone());
			continue;
		}
	}
	for disabled in disabled_external {
		external.remove(&disabled);
	}
	if !external.is_empty() {
		// TODO: use unicode width?
		let longest = external.keys().map(|n| n.len()).max().expect("not empty");
		let after = match cmd.get_after_help() {
			Some(v) => color_print::cformat!("\n\n{}", v.ansi()),
			None => String::new(),
		};
		let mut full: String =
			color_print::cformat!("<bold><underline>External commands:</underline></bold>");
		for (name, path) in &external {
			let line = color_print::cformat!(
				// Having padding on name, instead of manually implementing it
				// causes some terminal formatting ugliness for me
				"\n  <bold>{name}</bold>{:<padding$}{}",
				"",
				path.display(),
				padding = longest + 2 - name.len()
			);
			full.push_str(&line);
		}

		full.push_str(&after);
		cmd = cmd.after_help(full);
	}
	let mut matches = match cmd.try_get_matches() {
		Ok(v) => v,
		Err(e) => e.exit(),
	};
	match matches.subcommand() {
		Some((name, args)) if external.contains_key(name) => {
			let args = args.get_many::<OsString>("").unwrap().collect::<Vec<_>>();
			let status = process::Command::new(external.get(name).expect("exists"))
				.args(&args)
				.status()
				.expect("failed to spawn exernal");
			process::exit(status.code().unwrap_or(1));
		}
		_ => {}
	}

	match T::from_arg_matches_mut(&mut matches) {
		Ok(opts) => opts,
		Err(e) => {
			let e = e.format(&mut Opts::command());
			e.exit()
		}
	}
}

fn main() -> anyhow::Result<()> {
	tracing_subscriber::fmt::init();
	let opts: Opts = parse_or_external();
	match opts {
		Opts::Init => {
			match fs::metadata("migrations") {
				Ok(_) => anyhow::bail!("migrations directory already exists"),
				Err(e) if e.kind() == ErrorKind::NotFound => {}
				Err(e) => return Err(e.into()),
			}
			fs::create_dir("migrations")?;
			fs::write("migrations/db.schema", "")?;
		}
		Opts::Commit {
			message,
			before_up_sql,
			after_up_sql,
			before_down_sql,
			after_down_sql,
		} => {
			let root = find_root(&current_dir()?)?;
			let list = list(&root)?;
			let id = list.last().map(|(id, _, _)| id.id + 1).unwrap_or_default();

			let (original_str, original, original_report, orig_rn) =
				stored_schema(&list).context("failed to load past migrations")?;

			let (current_str, current, current_report, current_rn) =
				current_schema(&root).context("failed to parse current schema")?;

			let mut rn = orig_rn;
			rn.merge(current_rn);

			let should_use_editor = message.is_none();

			let message = message.unwrap_or_default();
			let mut message = message.splitn(2, '\n');
			let name = message.next().expect("at least one");
			let description = message.next().unwrap_or("");

			let slug = slug::slugify(name);
			let id = MigrationId::new(id, slug);
			let mut migration = Migration::new(
				name.to_owned(),
				description.to_owned(),
				before_up_sql,
				after_up_sql,
				before_down_sql,
				after_down_sql,
				current_str.clone(),
			);

			if should_use_editor {
				bail!("$EDITOR usage is not yet supported")
				// let editor = env::var_os("EDITOR")
				// 	.ok_or_else(|| anyhow!("editor is requested, but $EDITOR is not set"))?;
				// let mut migration_str = migration.to_string();
				// loop {
				// 	let mut editorfile = tempfile::NamedTempFile::new()?;
				// 	editorfile.write_all(migration.to_string().as_bytes())?;
				// 	editorfile.flush()?;
				// 	let status = Command::new(&editor)
				// 		.arg(editorfile.path())
				// 		.status()
				// 		.context("spawn editor")?;
				// 	if !status.success() {
				// 		println!("Editor failed");
				// 		return Ok(());
				// 	}
				// 	editorfile.seek(std::io::SeekFrom::Start(0))?;
				// 	let mut new_contents = String::new();
				// 	editorfile.read_to_string(&mut new_contents)?;
				// }
			}

			let mut dir = root;
			dir.push(&id.dirname);

			fs::create_dir(&dir).context("creating migration directory")?;

			migration.to_diff(original_str.clone())?;

			if migration.is_noop() {
				println!("No changes found");
				return Ok(());
			}

			{
				let mut schema_update = dir.to_owned();
				schema_update.push("db.update");
				fs::write(schema_update, migration.to_string()).context("writing db.update")?;
			}

			generate_sql(
				&migration,
				&original_str,
				&current_str,
				&original,
				&current,
				&rn,
				&dir,
				original_report,
				current_report,
			)?;
		}
		Opts::Diff => {
			let root = find_root(&current_dir()?)?;
			let list = list(&root)?;

			let (original_str, original, mut original_report, orig_rn) =
				stored_schema(&list).context("failed to load past migrations")?;

			let (current_str, current, mut current_report, current_rn) =
				current_schema(&root).context("failed to parse current schema")?;

			let mut rn = orig_rn;
			rn.merge(current_rn);

			let mut up = String::new();

			Pg(&current).diff(
				&Pg(&original),
				&mut up,
				&mut rn.clone(),
				&mut original_report,
				&mut current_report,
			);
			if display_reports(&original_str, &current_str, original_report, current_report) {
				bail!("errors are reported, cannot continue.\nPartial output:\n{up}");
			}
			println!("{up}")
		}
		Opts::List => {
			let root = find_root(&current_dir()?)?;
			let list = list_ids(&root)?;
			for migration in list {
				println!("{}. {}", migration.id, migration.slug);
			}
		}
		Opts::RegenerateSql => {
			let mut root = find_root(&current_dir()?)?;
			let list = list(&root)?;
			let mut current_schema_str = String::new();
			let mut current_schema = Schema::default();
			let mut rn = RenameMap::default();
			for (id, migration, _) in list {
				let old_schema_str = current_schema_str.clone();
				current_schema_str = migration.apply_diff(current_schema_str)?;
				let mut crn = RenameMap::default();
				let (updated_schema, report) = parse_schema(&current_schema_str, true, &mut crn)?;
				rn.merge(crn);
				generator_postgres::validate::validate(&current_schema_str, &updated_schema, &rn);

				root.push(&id.dirname);
				let old_report = Report::new();
				generate_sql(
					&migration,
					&old_schema_str,
					&current_schema_str,
					&current_schema,
					&updated_schema,
					&rn,
					&root,
					old_report,
					report,
				)?;
				root.pop();

				current_schema = updated_schema;
			}
		}
	}

	Ok(())
}

/* #[cfg(test)]
mod tests {
	use super::parser::parse as root;
	fn showdiff(a: &Schema, b: &Schema) {
		println!();
		println!("// up.sql");
		let mut out = String::new();
		b.diff(a, &mut out);
		print!("{out}");

		println!("// down.sql");
		let mut out = String::new();
		a.diff(b, &mut out);
		print!("{out}")
	}
	use super::*;
	#[test]
	fn create_table() {
		let from = root(
			r#"
						scalar id "INTEGER";
						table Test {id;}
		"#,
		);
		let to = root("");
		showdiff(&from, &to);
	}
	#[test]
	fn correct_index() {
		let to = root(
			r#"
						scalar id "INTEGER";
						table Test {
							a "acorrect": id @primary_key;
							b "bcorrect": id @index;
						}
		"#,
		);
		let from = root("");
		showdiff(&from, &to);
	}

	#[test]
	fn rename_index() {
		let from = root(r#"scalar id "INTEGER"; table Test { id @index "test1"; }"#);
		let to = root(r#"scalar id "INTEGER"; table Test { id @index; }"#);
		showdiff(&from, &to);
	}
	#[test]
	fn change_type() {
		let from = root(r#"scalar id "VARCHAR(10)"; table Test { id; }"#);
		let to = root(r#"scalar id "VARCHAR(20)"; table Test { id; id2: id; }"#);
		showdiff(&from, &to);
	}

	#[test]
	fn nullable() {
		let from = root(r#"scalar id "INTEGER"; table Test { id?; }"#);
		let to = root(r#"scalar id "INTEGER"; table Test { id; }"#);
		showdiff(&from, &to);
	}

	#[test]
	fn should_recreate_index_on_type_change() {
		let from = root(r#"scalar id "VARCHAR(1)"; table Test { id @index; }"#);
		let to = root(r#"scalar id "VARCHAR(2)"; table Test { id @index; }"#);
		showdiff(&from, &to);
	}
	#[test]
	fn enums() {
		let from = root(
			r#"
			enum removed_enum{hello; world};
			enum altered_enum{a; b};
		"#,
		);
		let to = root(
			r#"
			enum added_enum{hello; world};
			enum altered_enum{b; c};
		"#,
		);
		showdiff(&from, &to);
	}

	#[test]
	fn added_constraint() {
		let from = root(
			r#"
			scalar int "INTEGER";
			table A {
				a: int @primary_key;
				b: int;
			};
		"#,
		);
		showdiff(&root(""), &from);
		let to = root(
			r#"
			scalar int "INTEGER";
			table A {
				a: int;
				b: int @primary_key;
			};
		"#,
		);
		showdiff(&from, &to);
	}
} */
