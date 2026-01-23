use std::{
	collections::HashMap,
	ffi::OsString,
	fs::{self, read_dir},
	io,
	path::{Path, PathBuf},
	result,
	str::FromStr,
};

mod migration;
mod patch_util;

use Error::*;

pub use self::migration::Migration;

#[derive(Debug)]
pub struct MigrationId {
	pub id: u32,
	pub slug: String,
	pub dirname: String,
}
impl MigrationId {
	pub fn new(id: u32, slug: String) -> Self {
		Self {
			id,
			dirname: format!("{id:0>14}_{slug}"),
			slug,
		}
	}
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error("io: {0}")]
	Io(#[from] io::Error),
	#[error("file name is not utf-8: {0:?}")]
	NonUtf8(OsString),

	#[error("missing migration id: {0:?}, dir name should start with number, i.e 00003-name")]
	MissingMigrationId(String),
	#[error("missing migration name: {0:?}, dir name should end with short name, i.e 00003-name")]
	MissingSlug(String),

	#[error("two migrations have the same number: {0:?} and {1:?}")]
	SequenceIdConflict(String, String),
	#[error("missing migration with id {0}")]
	IdHole(u32),

	#[error(
		"failed to find migrations directory at {0} or any parent directory. Have you forgot to init your project?"
	)]
	FailedToFindRoot(PathBuf),

	#[error("failed to parse migration {id}: {error}")]
	MigrationParse {
		id: String,
		#[source]
		error: migration::Error,
	},

	#[error("failed to read db.update")]
	MigrationReadError(io::Error),
}
pub type Result<T, E = Error> = result::Result<T, E>;

pub fn list_ids(path: &Path) -> Result<Vec<MigrationId>> {
	let dir = read_dir(path)?;
	let mut ids = Vec::new();
	for ele in dir {
		let ele = ele?;
		let meta = ele.metadata()?;
		if !meta.is_dir() {
			continue;
		}
		let dirname = ele.file_name();
		let dirname = dirname.to_str().ok_or_else(|| NonUtf8(dirname.clone()))?;

		let mut split = dirname.splitn(2, '_');
		let Some(id) = split.next() else {
			return Err(MissingMigrationId(dirname.to_owned()));
		};
		let Ok(id) = id.parse::<u32>() else {
			return Err(MissingMigrationId(dirname.to_owned()));
		};
		let Some(slug) = split.next() else {
			return Err(MissingSlug(dirname.to_owned()));
		};

		ids.push(MigrationId {
			id,
			slug: slug.to_string(),
			dirname: dirname.to_owned(),
		});
	}
	if ids.is_empty() {
		return Ok(Vec::new());
	}
	ids.sort_by_key(|id| id.id);
	// Disallow duplicates
	{
		let mut has = HashMap::new();
		for id in ids.iter() {
			if let Some(old) = has.insert(id.id, id.slug.clone()) {
				return Err(SequenceIdConflict(old, id.slug.to_owned()));
			}
		}
	}
	// Disallow holes
	{
		for (i, id) in ids.iter().enumerate() {
			if id.id as usize != i {
				return Err(IdHole(i as u32));
			}
		}
	}
	Ok(ids)
}
pub fn list(root: &Path) -> Result<Vec<(MigrationId, Migration, PathBuf)>> {
	let mut out = Vec::new();
	let ids = list_ids(root)?;
	let mut path = root.to_path_buf();
	for id in ids {
		let slug = id.slug.clone();
		path.push(&id.dirname);
		let migration_dir = path.to_owned();
		path.push("db.update");

		let update = fs::read_to_string(&path).map_err(Error::MigrationReadError)?;
		let migration =
			Migration::from_str(&update).map_err(|error| MigrationParse { id: slug, error })?;
		out.push((id, migration, migration_dir));

		path.pop();
		path.pop();
	}
	Ok(out)
}

pub fn find_root(from: &Path) -> Result<PathBuf> {
	let mut out = from.to_path_buf();
	loop {
		out.push("migrations");
		if out.is_dir() {
			return Ok(out);
		}
		out.pop();
		if !out.pop() {
			return Err(FailedToFindRoot(from.to_owned()));
		}
	}
}
