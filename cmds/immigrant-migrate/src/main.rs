#![allow(
	clippy::get_first,
	reason = "vec.first() is being resolved as RunQueryDsl, thus .get(0) should be used: https://github.com/diesel-rs/diesel_async/issues/142, https://github.com/rust-lang/rust/issues/127306"
)]
use std::{env, env::current_dir, fs};

use anyhow::{Context, Result, bail};
use clap::Parser;
use cli::{current_schema, display_reports, generate_sql, generate_sql_nowrite, stored_schema};
use diesel::sql_query;
use diesel::sql_types::{Integer, Nullable, Text};
use diesel_async::{
	AnsiTransactionManager, AsyncConnection as _, AsyncPgConnection, RunQueryDsl as _,
	SimpleAsyncConnection as _, TransactionManager as _,
};
use file_diffs::{Migration, MigrationId, find_root, list};
use tracing::{error, warn};

#[derive(Parser)]
enum Subcommand {
	/// Execute migrations, execute and commit pending migration
	Commit {
		/// How to name the change.
		///
		/// If not set - commit editor will be opened.
		// FIXME: Duplicates way too many code from `immigrant commit`
		#[clap(long, short = 'm')]
		message: Option<String>,
		/// What SQL code should be added before executing the immigrant diff.
		#[clap(long)]
		before_up_sql: Option<String>,
		/// What SQL code should be added after executing the immigrant diff.
		#[clap(long)]
		after_up_sql: Option<String>,
		/// What SQL code should be added before reverting the immigrant diff.
		#[clap(long)]
		before_down_sql: Option<String>,
		/// What SQL code should be added after reverting the immigrant diff.
		#[clap(long)]
		after_down_sql: Option<String>,
		/// If set in dry-run mode, pending migration is written to disk on success, otherwise it
		/// will only be checked.
		#[clap(long)]
		write: bool,
	},
	/// Execute migrations
	Apply,
}

/// Immigrant helper to apply migrations to database
#[derive(Parser)]
#[clap(author, version)]
struct Opts {
	/// Which table should be used to keep immigrant own migration apply status
	#[arg(long, default_value = "__immigrant_migrations")]
	migrations_table: String,
	/// If set - sql is executed in transaction, which is rolled back immediately.
	#[clap(long)]
	dry_run: bool,
	/// If migration schema was changed on disk for some reason - migration will fail
	/// this argument allows to ignore such migrations.
	#[clap(long)]
	unsafe_override_mismatched: Vec<u32>,
	/// Action to execute
	#[command(subcommand)]
	cmd: Subcommand,
}

async fn run_migrations(
	conn: &mut AsyncPgConnection,
	id: u32,
	migration: String,
	migrations_table: &str,
	schema_str: &str,
) -> Result<()> {
	AnsiTransactionManager::begin_transaction(conn).await?;

	let result: Result<(), diesel::result::Error> = async {
		sql_query(format!(
			"INSERT INTO {migrations_table}(version, schema) VALUES ($1, $2);"
		))
		.bind::<Integer, _>(id as i32)
		.bind::<Text, _>(schema_str)
		.execute(conn)
		.await?;
		conn.batch_execute(&migration).await?;
		Ok(())
	}
	.await;

	match result {
		Ok(()) => {
			AnsiTransactionManager::commit_transaction(conn).await?;
			Ok(())
		}
		Err(e) => {
			AnsiTransactionManager::rollback_transaction(conn).await?;
			Err(e.into())
		}
	}
}

#[derive(diesel::QueryableByName)]
struct RanMigration {
	#[diesel(sql_type = Integer)]
	version: i32,
	#[diesel(sql_type = Nullable<Text>)]
	schema: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
	tracing_subscriber::fmt::init();
	let opts = Opts::parse();

	let migrations_table = &opts.migrations_table;

	let database_url = env::var("DATABASE_URL")?;
	let mut conn = AsyncPgConnection::establish(&database_url).await?;
	conn.batch_execute(&format!(
		r#"
				CREATE TABLE IF NOT EXISTS {migrations_table} (
					version INTEGER NOT NULL PRIMARY KEY,
					run_on TIMESTAMP NOT NULL DEFAULT NOW(),
					-- Either <noop>, <reset>%fullImmigrantSchema, or <diff>%diffImmigrantSchema
					schema TEXT
				);
				ALTER TABLE {migrations_table} ADD COLUMN IF NOT EXISTS schema TEXT;
			"#
	))
	.await?;
	let mut ran_migrations: Vec<RanMigration> =
		sql_query("SELECT version, schema FROM __immigrant_migrations ORDER BY version")
			.load(&mut conn)
			.await?;
	for migration in ran_migrations.iter() {
		assert!(migration.version >= 0);
	}
	let first_version = ran_migrations.get(0).map(|m| m.version as u32);
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

	// TODO: Option to disable top-level transaction
	AnsiTransactionManager::begin_transaction(&mut conn).await?;
	let root = find_root(&current_dir()?).context("failed to discover root")?;
	let list = list(&root).context("failed to list migrations")?;

	'next_migration: for (id, schema, path) in &list {
		let check_str = schema.schema_check_string();
		'run_migration: {
			let Some(first_version) = first_version else {
				break 'run_migration;
			};
			let Some(ran_id) = id.id.checked_sub(first_version) else {
				continue 'next_migration;
			};
			let Some(migration) = ran_migrations.get_mut(ran_id as usize) else {
				break 'run_migration;
			};
			let Some(expected_schema) = &migration.schema else {
				continue 'next_migration;
			};
			if expected_schema.trim() != check_str.trim() {
				if opts.unsafe_override_mismatched.contains(&id.id) {
					warn!("overriding migration {id:?}");
					sql_query("UPDATE __immigrant_migrations SET schema = $1 WHERE version = $2")
						.bind::<Text, _>(&check_str)
						.bind::<Integer, _>(id.id as i32)
						.execute(&mut conn)
						.await?;
					migration.schema = Some(check_str);
				} else {
					had_mismatched_migrations = true;
					mismatched_ids.push(id.id);
					error!(
						"schema, stored in DB, doesn't match the schema stored locally!\n\nLocal\n=====\n{check_str}\n\n\n\nRemote\n======\n{expected_schema}"
					);
				}
			} else if opts.unsafe_override_mismatched.contains(&id.id) {
				bail!("migration is valid, but it is specified in --unsafe-override-mismatched")
			}
			continue 'next_migration;
		}
		if had_mismatched_migrations {
			bail!(
				"mismatched migrations found, can't continue with applying rest of local-only migrations\nMismatched: {mismatched_ids:?}"
			);
		}
		let mut path = path.to_owned();
		path.push("up.sql");
		let sql = fs::read_to_string(&path).context("reading migration up.sql file")?;
		run_migrations(&mut conn, id.id, sql, migrations_table, &check_str).await?;
	}
	if had_mismatched_migrations {
		bail!(
			"mismatched migrations found, can't continue with new migration generation\nMismatched: {mismatched_ids:?}"
		);
	}
	let id = list.last().map(|(id, _, _)| id.id + 1).unwrap_or_default();

	let (original_str, original, mut original_report, orig_rn) =
		stored_schema(&list).context("failed to load past migrations")?;

	let (current_str, current, mut current_report, current_rn) =
		current_schema(&root).context("failed to parse current schema")?;

	let mut rn = orig_rn;
	rn.merge(current_rn);

	match opts.cmd {
		Subcommand::Apply => {
			let mut migration = Migration::new(
				"pending_check".to_owned(),
				"pending_check".to_owned(),
				None,
				None,
				None,
				None,
				current_str.clone(),
			);

			migration.to_diff(original_str.clone())?;

			if !migration.is_noop() {
				bail!("Dirty database schema, not applying");
			}
			if !opts.dry_run {
				AnsiTransactionManager::commit_transaction(&mut conn).await?;
			} else {
				println!("Dry-run succeeded");
			}
		}
		Subcommand::Commit {
			message,
			before_up_sql,
			after_up_sql,
			before_down_sql,
			after_down_sql,
			write,
		} => {
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
				if !opts.dry_run {
					AnsiTransactionManager::commit_transaction(&mut conn).await?;
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
			if display_reports(
				&original_str,
				&current_str,
				original_report.clone(),
				current_report.clone(),
			) {
				bail!("errors are reported, cannot continue");
			}
			if let Err(e) = run_migrations(
				&mut conn,
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

			if !opts.dry_run {
				fs::create_dir(&dir).context("creating migration directory")?;

				let mut schema_update = dir.to_owned();
				schema_update.push("db.update");
				fs::write(schema_update, migration.to_string()).context("writing db.update")?;
			}
			if !opts.dry_run {
				AnsiTransactionManager::commit_transaction(&mut conn).await?;
			} else {
				println!("Dry-run succeeded");
			}
			if opts.dry_run && write {
				fs::create_dir(&dir).context("creating migration directory")?;

				let mut schema_update = dir.to_owned();
				schema_update.push("db.update");
				fs::write(schema_update, migration.to_string()).context("writing db.update")?;
			}
			if !opts.dry_run || write {
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
