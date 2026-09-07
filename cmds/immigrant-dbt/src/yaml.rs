use std::fmt::Write;

pub enum Yaml {
	Str(String),
	Int(i64),
	Bool(bool),
	Seq(Vec<Yaml>),
	Map(Vec<(String, Yaml)>),
}

impl Yaml {
	pub fn str(v: impl Into<String>) -> Self {
		Self::Str(v.into())
	}
	pub fn map() -> Self {
		Self::Map(Vec::new())
	}
	pub fn with(mut self, key: impl Into<String>, value: Yaml) -> Self {
		self.push(key, value);
		self
	}
	pub fn push(&mut self, key: impl Into<String>, value: Yaml) {
		match self {
			Self::Map(entries) => entries.push((key.into(), value)),
			_ => panic!("push on non-map yaml"),
		}
	}
	pub fn strs<'a>(items: impl IntoIterator<Item = &'a str>) -> Self {
		Self::Seq(items.into_iter().map(Yaml::str).collect())
	}
	pub fn render(&self) -> String {
		let mut out = String::new();
		self.write_block(0, &mut out);
		out
	}
	fn is_scalar(&self) -> bool {
		matches!(self, Self::Str(_) | Self::Int(_) | Self::Bool(_))
	}
	fn is_empty(&self) -> bool {
		match self {
			Self::Seq(v) => v.is_empty(),
			Self::Map(v) => v.is_empty(),
			_ => false,
		}
	}
	fn write_scalar(&self, out: &mut String) {
		match self {
			Self::Str(v) => write_string(v, out),
			Self::Int(v) => write!(out, "{v}").expect("string"),
			Self::Bool(v) => write!(out, "{v}").expect("string"),
			Self::Seq(_) => out.push_str("[]"),
			Self::Map(_) => out.push_str("{}"),
		}
	}
	fn write_block(&self, indent: usize, out: &mut String) {
		let pad = " ".repeat(indent);
		match self {
			Self::Map(entries) => {
				for (i, (key, value)) in entries.iter().enumerate() {
					if i != 0 {
						out.push_str(&pad);
					}
					write_key(key, out);
					out.push(':');
					self.write_value(value, indent, out);
				}
			}
			Self::Seq(items) => {
				for (i, item) in items.iter().enumerate() {
					if i != 0 {
						out.push_str(&pad);
					}
					out.push('-');
					self.write_value(item, indent, out);
				}
			}
			_ => {
				self.write_scalar(out);
				out.push('\n');
			}
		}
	}
	fn write_value(&self, value: &Yaml, indent: usize, out: &mut String) {
		if value.is_scalar() || value.is_empty() {
			out.push(' ');
			value.write_scalar(out);
			out.push('\n');
		} else if matches!(self, Self::Seq(_)) && matches!(value, Self::Map(_)) {
			out.push(' ');
			value.write_block(indent + 2, out);
		} else {
			out.push('\n');
			out.push_str(&" ".repeat(indent + 2));
			value.write_block(indent + 2, out);
		}
	}
}

fn write_key(key: &str, out: &mut String) {
	let plain = !key.is_empty()
		&& key
			.chars()
			.next()
			.is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
		&& key
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
	if plain {
		out.push_str(key);
	} else {
		write_string(key, out);
	}
}

fn write_string(v: &str, out: &mut String) {
	out.push('"');
	for c in v.chars() {
		match c {
			'"' => out.push_str("\\\""),
			'\\' => out.push_str("\\\\"),
			'\n' => out.push_str("\\n"),
			'\r' => out.push_str("\\r"),
			'\t' => out.push_str("\\t"),
			c if (c as u32) < 0x20 => write!(out, "\\u{:04x}", c as u32).expect("string"),
			c => out.push(c),
		}
	}
	out.push('"');
}

#[cfg(test)]
mod tests {
	use super::Yaml;

	#[test]
	fn nested() {
		let doc = Yaml::map().with("version", Yaml::Int(2)).with(
			"models",
			Yaml::Seq(vec![
				Yaml::map()
					.with("name", Yaml::str("a: \"b\"\nc"))
					.with("+materialized", Yaml::str("view"))
					.with("columns", Yaml::Seq(vec![]))
					.with("tests", Yaml::strs(["not_null", "unique"])),
			]),
		);
		let expected = "version: 2\nmodels:\n  - name: \"a: \\\"b\\\"\\nc\"\n    \"+materialized\": \"view\"\n    columns: []\n    tests:\n      - \"not_null\"\n      - \"unique\"\n";
		assert_eq!(doc.render(), expected);
	}
}
