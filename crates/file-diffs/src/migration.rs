use std::{
	collections::BTreeMap,
	fmt,
	iter::Peekable,
	result,
	str::{FromStr, Lines},
};

use Error::*;
use imara_diff::{Algorithm, BasicLineDiffPrinter, Diff, InternedInput, UnifiedDiffConfig};
use tracing::trace;

use crate::patch_util::{self, OwnedPatch};

pub const ROOT_FILE: &str = "db.schema";

pub type Sources = BTreeMap<String, String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
	pub roots: Vec<String>,
	pub files: Sources,
}
impl Default for Snapshot {
	fn default() -> Self {
		Self {
			roots: vec![ROOT_FILE.to_owned()],
			files: Sources::new(),
		}
	}
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error("migration patch failed: {0}")]
	PatchFailed(#[from] patch_util::Error),
	#[error("update is empty")]
	UpdateIsEmpty,
	#[error(
		"file should start with the header, which is prefixed by '# ' (notice it should have a space after sharp)"
	)]
	MissingHeader,
	#[error("unexpected header: {0}")]
	UnexpectedHeader(String),
	#[error("invalid schema version: {0}")]
	InvalidSchemaVersion(String),
	#[error("expected ```immigrant code block")]
	MalformedCodeBlock,
	#[error("patch for {0:?} can't be applied, file doesn't exist")]
	MissingFile(String),
	#[error("root should be written as \"- path\": {0}")]
	MalformedRoot(String),
	#[error("non-destructive mode should be written as \"keep id\": {0}")]
	MalformedNonDestructive(String),
}
pub type Result<T, E = Error> = result::Result<T, E>;

enum FileChange {
	Added(String),
	Modified(OwnedPatch),
	Removed,
}

enum MigrationSchemaDiff {
	None,
	Reset(Sources),
	Diff(BTreeMap<String, FileChange>),
}
impl MigrationSchemaDiff {
	fn legacy_reset(&self) -> Option<&str> {
		match self {
			MigrationSchemaDiff::Reset(files) if files.len() == 1 => {
				files.get(ROOT_FILE).map(String::as_str)
			}
			_ => None,
		}
	}
	fn legacy_diff(&self) -> Option<&OwnedPatch> {
		match self {
			MigrationSchemaDiff::Diff(files) if files.len() == 1 => match files.get(ROOT_FILE) {
				Some(FileChange::Modified(patch)) => Some(patch),
				_ => None,
			},
			_ => None,
		}
	}
}

fn text_diff(old: &str, new: &str) -> Result<Option<OwnedPatch>> {
	let old = format!("{}\n", old.trim_end());
	let new = format!("{}\n", new.trim_end());
	let diff_input = InternedInput::new(old.as_str(), new.as_str());
	let mut diff = Diff::compute(Algorithm::Histogram, &diff_input);
	diff.postprocess_lines(&diff_input);
	let update = diff
		.unified_diff(
			&BasicLineDiffPrinter(&diff_input.interner),
			UnifiedDiffConfig::default(),
			&diff_input,
		)
		.to_string();
	if update.trim().is_empty() {
		return Ok(None);
	}
	Ok(Some(OwnedPatch::from_str(&update)?))
}

pub struct Migration {
	// id: MigrationId,
	name: String,
	description: String,
	pub schema_version: u32,
	roots: Option<Vec<String>>,
	schema_diff: MigrationSchemaDiff,
	pub keep_obsolete: Option<String>,
	pub before_up_sql: String,
	pub after_up_sql: String,
	pub before_down_sql: String,
	pub after_down_sql: String,
}
impl Migration {
	pub fn new(
		name: String,
		description: String,
		schema_version: u32,
		before_up: Option<String>,
		after_up: Option<String>,
		before_down: Option<String>,
		after_down: Option<String>,
		reset: Snapshot,
	) -> Self {
		let mut roots = reset.roots;
		roots.sort();
		roots.dedup();
		Self {
			name,
			description,
			schema_version,
			before_up_sql: before_up.unwrap_or_default(),
			after_up_sql: after_up.unwrap_or_default(),
			before_down_sql: before_down.unwrap_or_default(),
			after_down_sql: after_down.unwrap_or_default(),
			roots: Some(roots),
			schema_diff: MigrationSchemaDiff::Reset(reset.files),
			keep_obsolete: None,
		}
	}
	// Result the schema after this migration for daisy-chaining
	pub fn apply_diff(&self, old: Snapshot) -> Result<Snapshot> {
		Ok(Snapshot {
			roots: self.roots.clone().unwrap_or(old.roots),
			files: self.apply_files(old.files)?,
		})
	}
	fn apply_files(&self, mut old_schema: Sources) -> Result<Sources> {
		Ok(match &self.schema_diff {
			MigrationSchemaDiff::None => old_schema,
			MigrationSchemaDiff::Reset(schema) => {
				trace!("reset = {schema:?}");
				schema.clone()
			}
			MigrationSchemaDiff::Diff(diff) => {
				for (path, change) in diff {
					match change {
						FileChange::Added(text) => {
							old_schema.insert(path.clone(), text.clone());
						}
						FileChange::Modified(patch) => {
							trace!("diff {path} = {patch}");
							let old = old_schema
								.get_mut(path)
								.ok_or_else(|| MissingFile(path.clone()))?;
							*old = patch.apply(old)?;
						}
						FileChange::Removed => {
							old_schema.remove(path);
						}
					}
				}
				old_schema
			}
		})
	}
	// Convert reset schema to diff schema
	pub fn to_diff(&mut self, old: &Snapshot) -> Result<()> {
		if self.roots.as_ref() == Some(&old.roots) {
			self.roots = None;
		}
		let old_schema = &old.files;
		let MigrationSchemaDiff::Reset(reset) = &self.schema_diff else {
			return Ok(());
		};
		if old_schema.values().all(|v| v.trim().is_empty()) {
			return Ok(());
		}
		let mut changes = BTreeMap::new();
		for (path, new) in reset {
			match old_schema.get(path) {
				None => {
					changes.insert(path.clone(), FileChange::Added(new.clone()));
				}
				Some(old) => {
					if let Some(patch) = text_diff(old, new)? {
						changes.insert(path.clone(), FileChange::Modified(patch));
					}
				}
			}
		}
		for path in old_schema.keys() {
			if !reset.contains_key(path) {
				changes.insert(path.clone(), FileChange::Removed);
			}
		}
		self.schema_diff = if changes.is_empty() {
			MigrationSchemaDiff::None
		} else {
			MigrationSchemaDiff::Diff(changes)
		};
		Ok(())
	}
	pub fn is_noop(&self) -> bool {
		matches!(self.schema_diff, MigrationSchemaDiff::None)
			&& self.roots.is_none()
			&& self.before_up_sql.is_empty()
			&& self.after_up_sql.is_empty()
			&& self.before_down_sql.is_empty()
			&& self.after_down_sql.is_empty()
	}
	pub fn schema_check_string(&self) -> String {
		match &self.roots {
			Some(roots) => format!(
				"<roots>\n{}\n{}",
				roots.join("\n"),
				self.files_check_string()
			),
			None => self.files_check_string(),
		}
	}
	fn files_check_string(&self) -> String {
		if let Some(s) = self.schema_diff.legacy_reset() {
			return format!("<reset>\n{s}");
		}
		if let Some(d) = self.schema_diff.legacy_diff() {
			return format!("<diff>\n{d}");
		}
		match &self.schema_diff {
			MigrationSchemaDiff::None => "<noop>".to_string(),
			MigrationSchemaDiff::Reset(files) => {
				let mut out = "<reset>\n".to_owned();
				for (path, text) in files {
					out.push_str(&format!("### {path}\n{text}\n"));
				}
				out
			}
			MigrationSchemaDiff::Diff(files) => {
				let mut out = "<diff>\n".to_owned();
				for (path, change) in files {
					out.push_str(&match change {
						FileChange::Added(text) => format!("### {path} (new)\n{text}\n"),
						FileChange::Modified(patch) => format!("### {path}\n{patch}"),
						FileChange::Removed => format!("### {path} (removed)\n"),
					});
				}
				out
			}
		}
	}
}

fn write_code_block(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
	writeln!(f, "```immigrant")?;
	writeln!(f, "{text}")?;
	writeln!(f, "```")
}

fn parse_files<T>(
	l: &mut Peekable<Lines>,
	mut file: impl FnMut(&mut Peekable<Lines>, &str) -> Result<(String, T)>,
) -> Result<BTreeMap<String, T>> {
	let mut out = BTreeMap::new();
	loop {
		skip_blank(l);
		let Some(header) = l.next_if(|line| line.starts_with("### ")) else {
			break;
		};
		let (path, value) = file(l, &header["### ".len()..])?;
		out.insert(path, value);
	}
	Ok(out)
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

		skip_blank(&mut lines);
		let roots = if lines.next_if_eq(&"## Roots").is_some() {
			let mut roots = Vec::new();
			for line in until_next_header(&mut lines).lines() {
				let line = line.trim();
				if line.is_empty() {
					continue;
				}
				let root = line
					.strip_prefix("- ")
					.ok_or_else(|| MalformedRoot(line.to_owned()))?;
				roots.push(root.to_owned());
			}
			Some(roots)
		} else {
			None
		};

		let mut schema_version = 1;
		let schema_diff =
			if let Some(version) = next_if_versioned_header(&mut lines, "## Schema diff") {
				schema_version = version?;
				skip_blank(&mut lines);
				if lines.peek().is_some_and(|l| l.starts_with("### ")) {
					MigrationSchemaDiff::Diff(parse_files(&mut lines, |l, header| {
						if let Some(path) = header.strip_suffix(" (removed)") {
							Ok((path.to_owned(), FileChange::Removed))
						} else if let Some(path) = header.strip_suffix(" (new)") {
							skip_blank(l);
							Ok((path.to_owned(), FileChange::Added(code_block(l)?)))
						} else {
							let patch = OwnedPatch::from_str(&hunks_until_next_header(l))?;
							Ok((header.to_owned(), FileChange::Modified(patch)))
						}
					})?)
				} else {
					let patch = OwnedPatch::from_str(&until_next_header(&mut lines))?;
					MigrationSchemaDiff::Diff(BTreeMap::from([(
						ROOT_FILE.to_owned(),
						FileChange::Modified(patch),
					)]))
				}
			} else if let Some(version) = next_if_versioned_header(&mut lines, "## Schema reset") {
				schema_version = version?;
				if lines.peek() == Some(&"```immigrant") {
					MigrationSchemaDiff::Reset(BTreeMap::from([(
						ROOT_FILE.to_owned(),
						code_block(&mut lines)?,
					)]))
				} else {
					MigrationSchemaDiff::Reset(parse_files(&mut lines, |l, header| {
						skip_blank(l);
						Ok((header.to_owned(), code_block(l)?))
					})?)
				}
			} else {
				MigrationSchemaDiff::None
			};

		skip_blank(&mut lines);
		let keep_obsolete = if lines.next_if_eq(&"## Non-destructive").is_some() {
			let mode = until_next_header(&mut lines);
			let keep = mode
				.trim()
				.strip_prefix("keep ")
				.ok_or_else(|| MalformedNonDestructive(mode.clone()))?;
			Some(keep.trim().to_owned())
		} else {
			None
		};

		let mut section = |name: &str| {
			skip_blank(&mut lines);
			if lines.next_if_eq(&name).is_some() {
				until_next_header(&mut lines)
			} else {
				"".to_owned()
			}
		};
		let before_up_sql = section("## Before");
		let after_up_sql = section("## After");
		let before_down_sql = section("## Before (down)");
		let after_down_sql = section("## After (down)");
		if let Some(line) = lines.next()
			&& !line.is_empty()
		{
			return Err(UnexpectedHeader(line.to_owned()));
		}

		Ok(Migration {
			name,
			description,
			schema_version,
			roots,
			schema_diff,
			keep_obsolete,
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
		if let Some(roots) = &self.roots {
			writeln!(f)?;
			writeln!(f, "## Roots")?;
			for root in roots {
				writeln!(f, "- {root}")?;
			}
		}
		let version_suffix = if self.schema_version != 1 {
			format!(" (v{})", self.schema_version)
		} else {
			String::new()
		};
		if let Some(reset) = self.schema_diff.legacy_reset() {
			writeln!(f)?;
			writeln!(f, "## Schema reset{version_suffix}")?;
			write_code_block(f, reset)?;
			writeln!(f)?;
		} else if let Some(diff) = self.schema_diff.legacy_diff() {
			writeln!(f)?;
			writeln!(f, "## Schema diff{version_suffix}")?;
			write!(f, "{diff}")?;
		} else {
			match &self.schema_diff {
				MigrationSchemaDiff::None => {}
				MigrationSchemaDiff::Reset(files) => {
					writeln!(f)?;
					writeln!(f, "## Schema reset{version_suffix}")?;
					for (path, text) in files {
						writeln!(f)?;
						writeln!(f, "### {path}")?;
						write_code_block(f, text)?;
					}
					writeln!(f)?;
				}
				MigrationSchemaDiff::Diff(files) => {
					writeln!(f)?;
					writeln!(f, "## Schema diff{version_suffix}")?;
					for (path, change) in files {
						writeln!(f)?;
						match change {
							FileChange::Added(text) => {
								writeln!(f, "### {path} (new)")?;
								write_code_block(f, text)?;
							}
							FileChange::Modified(patch) => {
								writeln!(f, "### {path}")?;
								write!(f, "{patch}")?;
							}
							FileChange::Removed => writeln!(f, "### {path} (removed)")?,
						}
					}
				}
			}
		}
		if let Some(keep) = &self.keep_obsolete {
			writeln!(f)?;
			writeln!(f, "## Non-destructive")?;
			writeln!(f, "keep {keep}")?;
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
			writeln!(f, "{}", self.before_down_sql)?;
		}
		if !self.after_down_sql.is_empty() {
			writeln!(f)?;
			writeln!(f, "## After (down)")?;
			writeln!(f, "{}", self.after_down_sql)?;
		}

		Ok(())
	}
}

fn next_if_versioned_header(l: &mut Peekable<Lines>, name: &str) -> Option<Result<u32>> {
	let line = *l.peek()?;
	let rest = line.strip_prefix(name)?;
	let version = if rest.is_empty() {
		Ok(1)
	} else {
		let version = rest.strip_prefix(" (v").and_then(|r| r.strip_suffix(')'))?;
		version
			.parse()
			.map_err(|_| InvalidSchemaVersion(line.to_owned()))
	};
	l.next();
	Some(version)
}

fn skip_empty(l: &mut Peekable<Lines>) {
	if l.peek().map(|l| l.is_empty()).unwrap_or(false) {
		l.next();
	}
}
fn skip_blank(l: &mut Peekable<Lines>) {
	while l.next_if(|l| l.trim().is_empty()).is_some() {}
}
fn code_block(l: &mut Peekable<Lines>) -> Result<String> {
	let mut out = Vec::new();
	if l.next() != Some("```immigrant") {
		return Err(MalformedCodeBlock);
	}
	loop {
		let Some(line) = l.next_if(|l| *l != "```") else {
			break;
		};
		out.push(line);
	}
	if l.next() != Some("```") {
		return Err(MalformedCodeBlock);
	}
	Ok(out.join("\n"))
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
fn hunks_until_next_header(l: &mut Peekable<Lines>) -> String {
	let mut out = Vec::new();
	while let Some(line) = l.next_if(|l| !l.starts_with('#')) {
		out.push(line);
	}
	while out.last().is_some_and(|l| l.is_empty()) {
		out.pop();
	}
	out.join("\n")
}

#[cfg(test)]
mod tests {
	use std::str::FromStr;

	use super::{Migration, ROOT_FILE, Snapshot};

	fn sources(files: &[(&str, &str)]) -> Snapshot {
		Snapshot {
			files: files
				.iter()
				.map(|(k, v)| (k.to_string(), v.to_string()))
				.collect(),
			..Snapshot::default()
		}
	}

	fn migration(new: Snapshot, old: &Snapshot) -> Migration {
		let mut m = Migration::new(
			"test".to_owned(),
			String::new(),
			2,
			None,
			None,
			None,
			None,
			new,
		);
		m.to_diff(old).expect("diff");
		m
	}

	fn roundtrip(m: &Migration) -> Migration {
		let text = m.to_string();
		let parsed = Migration::from_str(&text).expect("parse");
		assert_eq!(parsed.to_string(), text);
		assert_eq!(parsed.schema_check_string(), m.schema_check_string());
		parsed
	}

	#[test]
	fn legacy_reset() {
		let text =
			"# init\n\n## Schema reset (v2)\n```immigrant\nscalar a = sql\"INTEGER\";\n```\n\n";
		let m = Migration::from_str(text).expect("parse");
		assert_eq!(m.to_string(), text);
		assert_eq!(
			m.schema_check_string(),
			"<reset>\nscalar a = sql\"INTEGER\";"
		);
		let m = migration(
			sources(&[(ROOT_FILE, "scalar a = sql\"INTEGER\";")]),
			&Snapshot::default(),
		);
		assert_eq!(m.to_string(), text.replace("# init", "# test"));
	}

	#[test]
	fn legacy_diff() {
		let old = sources(&[(ROOT_FILE, "a\nb\n")]);
		let new = sources(&[(ROOT_FILE, "a\nc\n")]);
		let m = migration(new.clone(), &old);
		assert_eq!(
			m.to_string(),
			"# test\n\n## Schema diff (v2)\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n"
		);
		assert_eq!(
			m.schema_check_string(),
			"<diff>\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n"
		);
		let parsed = roundtrip(&m);
		assert_eq!(parsed.apply_diff(old).unwrap(), new);
	}

	#[test]
	fn single_changed_root_in_multi_file_project_uses_legacy_layout() {
		let old = sources(&[(ROOT_FILE, "a\nb\n"), ("x.schema", "x\n")]);
		let new = sources(&[(ROOT_FILE, "a\nc\n"), ("x.schema", "x\n")]);
		let m = migration(new.clone(), &old);
		assert!(
			m.to_string()
				.ends_with("## Schema diff (v2)\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n")
		);
		assert_eq!(roundtrip(&m).apply_diff(old).unwrap(), new);
	}

	#[test]
	fn multi_file_diff() {
		let old = sources(&[
			(ROOT_FILE, "a\n"),
			("x.schema", "x1\ny\n"),
			("gone.schema", "g\n"),
		]);
		let new = sources(&[
			(ROOT_FILE, "a\n"),
			("x.schema", "x2\ny\n"),
			("new.schema", "#annotation\nn\n"),
		]);
		let m = migration(new.clone(), &old);
		assert_eq!(
			m.to_string(),
			"# test\n\n## Schema diff (v2)\n\n### gone.schema (removed)\n\n### new.schema (new)\n```immigrant\n#annotation\nn\n\n```\n\n### x.schema\n@@ -1,2 +1,2 @@\n-x1\n+x2\n y\n"
		);
		assert_eq!(
			m.schema_check_string(),
			"<diff>\n### gone.schema (removed)\n### new.schema (new)\n#annotation\nn\n\n### x.schema\n@@ -1,2 +1,2 @@\n-x1\n+x2\n y\n"
		);
		assert_eq!(roundtrip(&m).apply_diff(old).unwrap(), new);
	}

	#[test]
	fn hunk_ending_with_blank_context() {
		let old = sources(&[
			(ROOT_FILE, "a\nb\nc\n\n\n\nd\ne\nf\ng\n"),
			("x.schema", "x\n"),
		]);
		let new = sources(&[
			(ROOT_FILE, "a\nB\nc\n\n\n\nd\ne\nf\ng\n"),
			("x.schema", "y\n"),
		]);
		let m = migration(new.clone(), &old);
		assert_eq!(roundtrip(&m).apply_diff(old).unwrap(), new);
	}

	#[test]
	fn multi_file_reset() {
		let new = sources(&[(ROOT_FILE, "a\n"), ("x.schema", "x\n")]);
		let m = migration(new.clone(), &Snapshot::default());
		assert_eq!(
			m.to_string(),
			"# test\n\n## Schema reset (v2)\n\n### db.schema\n```immigrant\na\n\n```\n\n### x.schema\n```immigrant\nx\n\n```\n\n"
		);
		assert_eq!(roundtrip(&m).apply_diff(Snapshot::default()).unwrap(), new);
	}

	#[test]
	fn roots() {
		let plugins = |main: &str| Snapshot {
			roots: vec!["@main/db.schema".to_owned(), "@logseq/db.schema".to_owned()],
			files: [
				("@main/db.schema".to_owned(), main.to_owned()),
				("@logseq/db.schema".to_owned(), "b\n".to_owned()),
			]
			.into(),
		};
		let initial = plugins("a\n");
		let m = migration(initial.clone(), &Snapshot::default());
		assert_eq!(
			m.to_string(),
			"# test\n\n## Roots\n- @logseq/db.schema\n- @main/db.schema\n\n## Schema reset (v2)\n\n### @logseq/db.schema\n```immigrant\nb\n\n```\n\n### @main/db.schema\n```immigrant\na\n\n```\n\n"
		);
		assert!(
			m.schema_check_string()
				.starts_with("<roots>\n@logseq/db.schema\n@main/db.schema\n<reset>\n")
		);
		let applied = roundtrip(&m).apply_diff(Snapshot::default()).unwrap();
		assert_eq!(applied.files, initial.files);
		assert_eq!(applied.roots, vec!["@logseq/db.schema", "@main/db.schema"]);

		let changed = plugins("c\n");
		let m = migration(changed.clone(), &applied);
		assert!(!m.to_string().contains("## Roots"));
		assert_eq!(
			roundtrip(&m).apply_diff(applied.clone()).unwrap().files,
			changed.files
		);

		let fewer = Snapshot {
			roots: vec!["@main/db.schema".to_owned()],
			files: changed.files.clone(),
		};
		let m = migration(
			fewer.clone(),
			&Snapshot {
				roots: applied.roots,
				files: changed.files,
			},
		);
		assert!(!m.is_noop());
		assert_eq!(m.schema_check_string(), "<roots>\n@main/db.schema\n<noop>");
		let parsed = roundtrip(&m);
		assert_eq!(
			parsed.apply_diff(Snapshot::default()).unwrap().roots,
			fewer.roots
		);
	}

	#[test]
	fn keep_obsolete() {
		let old = sources(&[(ROOT_FILE, "a\nb\n")]);
		let mut m = migration(sources(&[(ROOT_FILE, "a\n")]), &old);
		m.keep_obsolete = Some("12".to_owned());
		m.before_up_sql = "SELECT 1;".to_owned();
		let text = m.to_string();
		assert!(text.ends_with("\n## Non-destructive\nkeep 12\n\n## Before\nSELECT 1;\n"));
		assert_eq!(roundtrip(&m).keep_obsolete.as_deref(), Some("12"));
	}

	#[test]
	fn sections_after_schema() {
		let text = "# init\n\n## Schema reset\n```immigrant\na\n```\n\n\n## Before\nSELECT 1;\n\n## After (down)\nSELECT 2;\n";
		let m = Migration::from_str(text).expect("parse");
		assert_eq!(m.before_up_sql, "SELECT 1;");
		assert_eq!(m.after_down_sql, "SELECT 2;");
		assert!(m.to_string().ends_with("## After (down)\nSELECT 2;\n"));
	}
}
