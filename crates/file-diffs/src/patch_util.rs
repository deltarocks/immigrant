use std::{fmt, result, str::FromStr};

use Error::*;
use ouroboros::self_referencing;
use patch::{Line, Patch};

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error("{error}\nfor patch: {input}")]
	Parse { error: String, input: String },
	#[error("failed to find context at {line_num}: {line}")]
	ContextNotFound { line_num: u64, line: String },
	#[error("remove not matched at {line_num}: {line}")]
	RemoveNoMatch { line_num: u64, line: String },
}
pub type Result<T, E = Error> = result::Result<T, E>;

#[self_referencing]
pub struct OwnedPatch {
	text: String,
	#[borrows(text)]
	#[not_covariant]
	patch: Patch<'this>,
}

impl OwnedPatch {
	pub fn apply(&self, input: &str) -> Result<String> {
		self.with_patch(|patch| apply_patch(patch, input))
	}
}

impl FromStr for OwnedPatch {
	type Err = Error;
	fn from_str(input: &str) -> Result<Self, Self::Err> {
		let mut input = input.to_owned();
		input.insert_str(0, "--- a\n+++ b\n");
		input.push('\n');
		// I have no idea how to make OwnedPatchTryBuilder work with this annotation
		let _ = Patch::from_single(&input).map_err(|mut e| {
			e.line -= 2;
			Parse {
				error: e.to_string(),
				input: input.clone(),
			}
		})?;
		Ok(OwnedPatchBuilder {
			text: input,
			patch_builder: |text: &String| {
				Patch::from_single(text.as_str())
					.expect("first parse is for check, second is for store")
			},
		}
		.build())
	}
}
impl fmt::Display for OwnedPatch {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.with_patch(|patch| {
			for hunk in &patch.hunks {
				writeln!(f, "{hunk}")?;
			}
			if !patch.end_newline {
				writeln!(f, "\\ No newline at the end of file")?;
			}
			Ok(())
		})
	}
}
fn apply_patch(diff: &Patch, old: &str) -> Result<String> {
	let old_lines = old.lines().collect::<Vec<&str>>();
	let mut out: Vec<&str> = vec![];
	let mut old_line = 0;
	for hunk in &diff.hunks {
		while hunk.old_range.start != 0 && old_line < hunk.old_range.start - 1 {
			out.push(old_lines[old_line as usize]);
			old_line += 1;
		}
		for line in &hunk.lines {
			match line {
				Line::Context(line) => {
					let old = old_lines.get(old_line as usize);
					if old != Some(line) {
						return Err(ContextNotFound {
							line_num: old_line,
							line: old
								.map(|s| s.to_string())
								.unwrap_or_else(|| "<unknown>".to_owned()),
						});
					}
					if (old_line as usize) < old_lines.len() {
						out.push(line);
					}
					old_line += 1;
				}
				Line::Add(s) => out.push(s),
				Line::Remove(line) => {
					let old = old_lines[old_line as usize];
					if &old != line {
						return Err(RemoveNoMatch {
							line_num: old_line,
							line: old.to_string(),
						});
					}
					old_line += 1;
				}
			}
		}
	}
	for line in old_lines.get((old_line as usize)..).unwrap_or(&[]) {
		out.push(line);
	}
	if old.ends_with('\n') {
		out.push("");
	}
	Ok(out.join("\n"))
}
