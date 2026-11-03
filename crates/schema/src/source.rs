use std::{collections::BTreeMap, fmt};

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ResolvedSpec {
	rendered: String,
	package: Option<String>,
	path: String,
}

impl ResolvedSpec {
	fn new(package: Option<String>, path: String) -> Self {
		let rendered = match &package {
			Some(package) => format!("{package}/{path}"),
			None => path.clone(),
		};
		Self {
			rendered,
			package,
			path,
		}
	}
	fn package_spec(spec: &str) -> Result<Self, String> {
		let (package, path) = spec.split_once('/').ok_or_else(|| {
			format!("package path {spec:?} should point to a file inside of package")
		})?;
		if package.len() < 2 {
			return Err(format!("package path {spec:?} has empty package name"));
		}
		let path = normalize(Vec::new(), path, true)?;
		Ok(Self::new(Some(package.to_owned()), path))
	}
	pub fn root(path: &str) -> Result<Self, String> {
		if path.starts_with('@') {
			return Self::package_spec(path);
		}
		if path.starts_with('/') {
			return Err(format!("root path {path:?} should be relative"));
		}
		normalize(Vec::new(), path, false).map(|path| Self::new(None, path))
	}
	pub fn resolve(&self, spec: &str) -> Result<Self, String> {
		if spec.starts_with('@') {
			return Self::package_spec(spec);
		}
		if !(spec.starts_with("./") || spec.starts_with("../")) {
			return Err(format!(
				"import path {spec:?} should be relative, starting with \"./\" or \"../\", or point to a package, starting with \"@\""
			));
		}
		let mut dir: Vec<&str> = self.path.split('/').collect();
		dir.pop();
		let path = normalize(dir, spec, self.package.is_some())?;
		Ok(Self::new(self.package.clone(), path))
	}
	pub fn package(&self) -> Option<&str> {
		self.package.as_deref()
	}
	pub fn path(&self) -> &str {
		&self.path
	}
	pub fn as_str(&self) -> &str {
		&self.rendered
	}
}

impl fmt::Display for ResolvedSpec {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.rendered)
	}
}

fn normalize<'a>(
	mut parts: Vec<&'a str>,
	path: &'a str,
	in_package: bool,
) -> Result<String, String> {
	for segment in path.split('/') {
		match segment {
			"" | "." => {}
			".." => {
				if parts.last().is_none_or(|p| *p == "..") {
					if in_package {
						return Err(format!("path {path:?} points outside of the package"));
					}
					parts.push("..");
				} else {
					parts.pop();
				}
			}
			s => parts.push(s),
		}
	}
	if parts.last().is_none_or(|p| *p == "..") {
		return Err(format!("path {path:?} does not point to a file"));
	}
	Ok(parts.join("/"))
}

pub trait SourceLoader {
	fn load(&self, path: &ResolvedSpec) -> Result<String, String>;
}

impl SourceLoader for BTreeMap<String, String> {
	fn load(&self, path: &ResolvedSpec) -> Result<String, String> {
		self.get(path.as_str())
			.cloned()
			.ok_or_else(|| format!("file {:?} not found", path.as_str()))
	}
}

impl<T: SourceLoader + ?Sized> SourceLoader for &T {
	fn load(&self, path: &ResolvedSpec) -> Result<String, String> {
		(**self).load(path)
	}
}

#[cfg(test)]
mod tests {
	use super::ResolvedSpec;

	fn resolve(from: &str, spec: &str) -> Result<String, String> {
		ResolvedSpec::root(from)
			.unwrap()
			.resolve(spec)
			.map(|s| s.to_string())
	}

	#[test]
	fn root() {
		assert_eq!(
			ResolvedSpec::root("db.schema").unwrap().as_str(),
			"db.schema"
		);
		assert_eq!(
			ResolvedSpec::root("./a/../db.schema").unwrap().as_str(),
			"db.schema"
		);
		assert_eq!(
			ResolvedSpec::root("../x//db.schema").unwrap().as_str(),
			"../x/db.schema"
		);
		assert!(ResolvedSpec::root("/db.schema").is_err());
		assert!(ResolvedSpec::root("a/..").is_err());
	}

	#[test]
	fn relative() {
		assert_eq!(resolve("db.schema", "./a.schema").unwrap(), "a.schema");
		assert_eq!(
			resolve("db.schema", "./auth/users.schema").unwrap(),
			"auth/users.schema"
		);
		assert_eq!(
			resolve("auth/users.schema", "./roles.schema").unwrap(),
			"auth/roles.schema"
		);
		assert_eq!(
			resolve("auth/users.schema", "../db.schema").unwrap(),
			"db.schema"
		);
		assert_eq!(
			resolve("auth/users.schema", "./../x/./y.schema").unwrap(),
			"x/y.schema"
		);
	}

	#[test]
	fn outside_root() {
		assert_eq!(
			resolve("db.schema", "../billing/db.schema").unwrap(),
			"../billing/db.schema"
		);
		assert_eq!(
			resolve("../billing/db.schema", "./types.schema").unwrap(),
			"../billing/types.schema"
		);
		assert_eq!(
			resolve("../billing/db.schema", "../../db.schema").unwrap(),
			"../../db.schema"
		);
	}

	#[test]
	fn packages() {
		let root = ResolvedSpec::root("@logseq/db.schema").unwrap();
		assert_eq!(root.package(), Some("@logseq"));
		assert_eq!(root.path(), "db.schema");
		assert_eq!(
			resolve("@logseq/db.schema", "./pages.schema").unwrap(),
			"@logseq/pages.schema"
		);
		assert_eq!(
			resolve("@logseq/tables/pages.schema", "../db.schema").unwrap(),
			"@logseq/db.schema"
		);
		assert_eq!(
			resolve("@logseq/db.schema", "@main/db.schema").unwrap(),
			"@main/db.schema"
		);
		assert_eq!(
			resolve("db.schema", "@main/./a/../db.schema").unwrap(),
			"@main/db.schema"
		);
		assert!(resolve("@logseq/db.schema", "../main/db.schema").is_err());
		assert!(resolve("db.schema", "@main/../db.schema").is_err());
		assert!(resolve("db.schema", "@main").is_err());
		assert!(resolve("db.schema", "@/db.schema").is_err());
	}

	#[test]
	fn invalid() {
		assert!(resolve("db.schema", "a.schema").is_err());
		assert!(resolve("db.schema", "/a.schema").is_err());
		assert!(resolve("db.schema", "./").is_err());
		assert!(resolve("db.schema", "../").is_err());
	}
}
