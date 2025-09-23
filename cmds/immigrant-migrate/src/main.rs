use std::{env, env::current_dir, fs};

use anyhow::{bail, Context, Result};
use clap::Parser;
use cli::{current_schema, display_reports, generate_sql, generate_sql_nowrite, stored_schema};
use file_diffs::{find_root, list, Migration, MigrationId};
use futures::StreamExt;
use sqlx::{postgres::PgPoolOptions, Acquire, Executor, Postgres, Transaction};
use tracing::{error, warn};

#[derive(Parser)]
enum Subcommand {
	Commit {
		/// How to name the change.
		///
		/// If not set - commit editor will be opened.
		// FIXME: Duplicates way too many code from `immigrant commit`
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
		#[clap(long)]
		dry_run: bool,
		#[clap(long)]
		unsafe_override_mismatched: Vec<u32>,
		/// If true - migration should be saved to disk.
		/// Always set if not in dry-run mode, in dry-run it is disabled by default.
		#[clap(long)]
		write: bool,
	},
}
#[derive(Parser)]
#[clap(author, version)]
struct Opts {
	#[arg(long, default_value = "__immigrant_migrations")]
	migrations_table: String,
	#[command(subcommand)]
	cmd: Subcommand,
}

async fn run_migrations(
	conn: &mut Transaction<'_, Postgres>,
	id: u32,
	migration: String,
	migrations_table: &str,
	schema_str: &str,
) -> Result<()> {
	let mut tx = conn.begin().await?;

	tx.execute(
		sqlx::query(
			format!("INSERT INTO {migrations_table}(version, schema) VALUES ($1, $2);").as_str(),
		)
		.bind(id as i32)
		.bind(schema_str),
	)
	.await?;
	{
		let mut executing = tx.execute_many(migration.as_str());
		while let Some(v) = executing.next().await {
			let _res = v?;
		}
	}

	tx.commit().await?;
	Ok(())
}

#[derive(sqlx::FromRow)]
struct RanMigration {
	version: i32,
	schema: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
	tracing_subscriber::fmt::init();
	let opts = Opts::parse();

	let migrations_table = &opts.migrations_table;

	let database_url = env::var("DATABASE_URL")?;
	let pool = PgPoolOptions::new().connect(&database_url).await?;
	let mut conn = pool.acquire().await?;
	conn.execute(
		format!(
			r#"
				CREATE TABLE IF NOT EXISTS {migrations_table} (
					version INTEGER NOT NULL PRIMARY KEY,
					run_on TIMESTAMP NOT NULL DEFAULT NOW(),
					-- Either <noop>, <reset>%fullImmigrantSchema, or <diff>%diffImmigrantSchema
					schema TEXT
				);
				ALTER TABLE {migrations_table} ADD COLUMN IF NOT EXISTS schema TEXT;
			"#
		)
		.as_str(),
	)
	.await?;
	let mut ran_migrations = sqlx::query_as::<_, RanMigration>(
		"SELECT version, schema FROM __immigrant_migrations ORDER BY version",
	)
	.fetch_all(&mut *conn)
	.await?;
	for migration in ran_migrations.iter() {
		assert!(migration.version >= 0);
	}
	let first_version = ran_migrations.first().map(|m| m.version as u32);
	// Not all migrations might be recorded, but they must be continous
	for (ran, expected) in ran_migrations
		.iter()
		.enumerate()
		.map(|(i, m)| (m, i as u32 + first_version.expect("not empty")))
	{
		assert_eq!(ran.version as u32, expected, "unexpected migration version");
	}

	let mut had_mismatched_migrations = false;
	let mut mismatched_ids = vec![];

	match opts.cmd {
		Subcommand::Commit {
			message,
			before_up_sql,
			after_up_sql,
			before_down_sql,
			after_down_sql,
			dry_run,
			write,
			unsafe_override_mismatched,
		} => {
			// TODO: Option to disable top-level transaction
			let mut tx = conn.begin().await?;
			let root = find_root(&current_dir()?).context("failed to discover root")?;
			let list = list(&root).context("failed to list migrations")?;

			'next_migration: for (id, schema, path) in &list {
				let check_str = schema.schema_check_string();
				'run_migration: {
					let Some(first_version) = first_version else {
						// No migrations were ran yet,
						break 'run_migration;
					};
					let Some(ran_id) = id.id.checked_sub(first_version) else {
						// Migration version was removed from DB, yet it was run.
						continue 'next_migration;
					};
					let Some(migration) = ran_migrations.get_mut(ran_id as usize) else {
						// This migration was not yet ran
						break 'run_migration;
					};
					let Some(expected_schema) = &migration.schema else {
						// No stored expected schema, continue with migration
						continue 'next_migration;
					};
					if expected_schema.trim() != check_str.trim() {
						if unsafe_override_mismatched.contains(&id.id) {
							warn!("overriding migration {id:?}");
							sqlx::query(
								"UPDATE __immigrant_migrations SET schema = $1 WHERE version = $2",
							)
							.bind(&check_str)
							.bind(id.id as i32)
							.execute(&mut *tx)
							.await?;
							migration.schema = Some(check_str);
						} else {
							had_mismatched_migrations = true;
							mismatched_ids.push(id.id);
							error!("schema, stored in DB, doesn't match the schema stored locally!\n\nLocal\n=====\n{check_str}\n\n\n\nRemote\n======\n{expected_schema}");
						}
					} else if unsafe_override_mismatched.contains(&id.id) {
						bail!("migration is valid, but it is specified in --unsafe-override-mismatched")
					}
					continue 'next_migration;
				}
				if had_mismatched_migrations {
					bail!("mismatched migrations found, can't continue with applying rest of local-only migrations\nMismatched: {mismatched_ids:?}");
				}
				let mut path = path.to_owned();
				path.push("up.sql");
				let sql = fs::read_to_string(&path).context("reading migration up.sql file")?;
				run_migrations(&mut tx, id.id, sql, migrations_table, &check_str).await?;
			}
			if had_mismatched_migrations {
				bail!("mismatched migrations found, can't continue with new migration generation\nMismatched: {mismatched_ids:?}");
			}
			let id = list.last().map(|(id, _, _)| id.id + 1).unwrap_or_default();

			let (original_str, original, mut original_report, orig_rn) =
				stored_schema(&list).context("failed to load past migrations")?;

			let (current_str, current, mut current_report, current_rn) =
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
			}

			migration.to_diff(original_str.clone())?;

			if migration.is_noop() {
				println!("No changes found");
				// Still need to preserve previous migrations state.
				if !dry_run {
					tx.commit().await?;
				}
				return Ok(());
			}

			let (sql, _) = generate_sql_nowrite(
				&migration,
				&original,
				&current,
				&rn,
				&mut original_report,
				&mut current_report,
			)?;
			if display_reports(&original_str, &current_str, original_report.clone(), current_report.clone()) {
				bail!("errors are reported, cannot continue");
			}
			if let Err(e) = run_migrations(
				&mut tx,
				id.id,
				sql.clone(),
				migrations_table,
				&migration.schema_check_string(),
			)
			.await
			{
				eprintln!("Won't commit failed migration:\n\n{sql}");
				return Err(e);
			};

			let mut dir = root.clone();
			dir.push(&id.dirname);

			if !dry_run || write {
				fs::create_dir(&dir).context("creating migration directory")?;

				{
					let mut schema_update = dir.to_owned();
					schema_update.push("db.update");
					fs::write(schema_update, migration.to_string()).context("writing db.update")?;
				}
			}
			if !dry_run {
				tx.commit().await?;
			} else {
				println!("Dry-run succeeded");
			}
			if !dry_run || write {
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
		}
	}
	Ok(())
}
