use std::{
	fmt,
	iter::Peekable,
	result,
	str::{FromStr, Lines},
};

use imara_diff::{Algorithm, BasicLineDiffPrinter, Diff, InternedInput, UnifiedDiffConfig};
use Error::*;

use crate::patch_util::{self, OwnedPatch};

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error("migration patch failed: {0}")]
	PatchFailed(#[from] patch_util::Error),
	#[error("update is empty")]
	UpdateIsEmpty,
	#[error("file should start with the header, which is prefixed by '# ' (notice it should have a space after sharp)")]
	MissingHeader,
	#[error("unexpected header: {0}")]
	UnexpectedHeader(String),
}
pub type Result<T, E = Error> = result::Result<T, E>;

enum MigrationSchemaDiff {
	None,
	Reset(String),
	Diff(OwnedPatch),
}

pub struct Migration {
	// id: MigrationId,
	name: String,
	description: String,
	schema_diff: MigrationSchemaDiff,
	pub before_up_sql: String,
	pub after_up_sql: String,
	pub before_down_sql: String,
	pub after_down_sql: String,
}
impl Migration {
	pub fn new(
		name: String,
		description: String,
		before_up: Option<String>,
		after_up: Option<String>,
		before_down: Option<String>,
		after_down: Option<String>,
		reset: String,
	) -> Self {
		Self {
			name,
			description,
			before_up_sql: before_up.unwrap_or_default(),
			after_up_sql: after_up.unwrap_or_default(),
			before_down_sql: before_down.unwrap_or_default(),
			after_down_sql: after_down.unwrap_or_default(),
			schema_diff: MigrationSchemaDiff::Reset(reset),
		}
	}
	// Result the schema after this migration for daisy-chaining
	pub fn apply_diff(&self, old_schema: String) -> Result<String> {
		Ok(match &self.schema_diff {
			MigrationSchemaDiff::None => old_schema,
			MigrationSchemaDiff::Reset(schema) => schema.to_owned(),
			MigrationSchemaDiff::Diff(diff) => diff.apply(&old_schema)?,
		})
	}
	// Convert reset schema to diff schema
	pub fn to_diff(&mut self, old_schema: String) -> Result<()> {
		let MigrationSchemaDiff::Reset(reset) = &self.schema_diff else {
			return Ok(());
		};
		let diff_input = InternedInput::new(old_schema.as_str(), reset.as_str());
		let diff = Diff::compute(Algorithm::Histogram, &diff_input);
		let update = diff
			.unified_diff(
				&BasicLineDiffPrinter(&diff_input.interner),
				UnifiedDiffConfig::default(),
				&diff_input,
			)
			.to_string();
		if update.trim().is_empty() {
			self.schema_diff = MigrationSchemaDiff::None;
		} else {
			let update = OwnedPatch::from_str(&update)?;
			self.schema_diff = MigrationSchemaDiff::Diff(update);
		}
		Ok(())
	}
	pub fn is_noop(&self) -> bool {
		matches!(self.schema_diff, MigrationSchemaDiff::None)
			&& self.before_up_sql.is_empty()
			&& self.after_up_sql.is_empty()
			&& self.before_down_sql.is_empty()
			&& self.after_down_sql.is_empty()
	}
	pub fn schema_check_string(&self) -> String {
		match &self.schema_diff {
			MigrationSchemaDiff::None => "<noop>".to_string(),
			MigrationSchemaDiff::Reset(s) => format!("<reset>\n{s}"),
			MigrationSchemaDiff::Diff(d) => format!("<diff>\n{d}"),
		}
	}
}
impl FromStr for Migration {
	type Err = Error;

	fn from_str(migration: &str) -> Result<Self> {
		let mut lines = migration.lines().peekable();
		skip_empty(&mut lines);
		let Some(header) = lines.next() else {
			return Err(UpdateIsEmpty);
		};
		let Some(name) = header.strip_prefix("# ") else {
			return Err(MissingHeader);
		};
		let name = name.to_owned();

		let description = until_next_header(&mut lines);

		let schema_diff = if lines.next_if_eq(&"## Schema diff").is_some() {
			let schema_diff = until_next_header(&mut lines);
			MigrationSchemaDiff::Diff(OwnedPatch::from_str(&schema_diff)?)
		} else if lines.next_if_eq(&"## Schema reset").is_some() {
			let schema_diff = until_next_header(&mut lines);
			MigrationSchemaDiff::Reset(schema_diff)
		} else {
			MigrationSchemaDiff::None
		};

		let before_up_sql = if lines.next_if_eq(&"## Before").is_some() {
			until_next_header(&mut lines)
		} else {
			"".to_owned()
		};
		let after_up_sql = if lines.next_if_eq(&"## After").is_some() {
			until_next_header(&mut lines)
		} else {
			"".to_owned()
		};
		let before_down_sql = if lines.next_if_eq(&"## Before (down)").is_some() {
			until_next_header(&mut lines)
		} else {
			"".to_owned()
		};
		let after_down_sql = if lines.next_if_eq(&"## After (down)").is_some() {
			until_next_header(&mut lines)
		} else {
			"".to_owned()
		};
		if let Some(line) = lines.next() {
			return Err(UnexpectedHeader(line.to_owned()));
		}

		Ok(Migration {
			name,
			description,
			schema_diff,
			before_up_sql,
			after_up_sql,
			before_down_sql,
			after_down_sql,
		})
	}
}
impl fmt::Display for Migration {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		writeln!(f, "# {}", self.name)?;
		if !self.description.is_empty() {
			writeln!(f, "{}", self.description)?;
		}
		match &self.schema_diff {
			MigrationSchemaDiff::None => {}
			MigrationSchemaDiff::Reset(reset) => {
				writeln!(f)?;
				writeln!(f, "## Schema reset")?;
				writeln!(f, "{reset}")?;
				writeln!(f)?;
			}
			MigrationSchemaDiff::Diff(diff) => {
				writeln!(f)?;
				writeln!(f, "## Schema diff")?;
				write!(f, "{diff}")?;
			}
		}
		if !self.before_up_sql.is_empty() {
			writeln!(f)?;
			writeln!(f, "## Before")?;
			writeln!(f, "{}", self.before_up_sql)?;
		}
		if !self.after_up_sql.is_empty() {
			writeln!(f)?;
			writeln!(f, "## After")?;
			writeln!(f, "{}", self.after_up_sql)?;
		}
		if !self.before_down_sql.is_empty() {
			writeln!(f)?;
			writeln!(f, "## Before (down)")?;
			writeln!(f, "{}", self.before_up_sql)?;
		}
		if !self.after_down_sql.is_empty() {
			writeln!(f)?;
			writeln!(f, "## After (down)")?;
			writeln!(f, "{}", self.after_up_sql)?;
		}

		Ok(())
	}
}

fn skip_empty(l: &mut Peekable<Lines>) {
	if l.peek().map(|l| l.is_empty()).unwrap_or(false) {
		l.next();
	}
}
fn until_next_header(l: &mut Peekable<Lines>) -> String {
	let mut out = Vec::new();
	skip_empty(l);
	loop {
		let Some(line) = l.next_if(|l| !l.starts_with('#')) else {
			break;
		};
		out.push(line);
	}
	while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
		out.pop();
	}
	out.join("\n")
}
