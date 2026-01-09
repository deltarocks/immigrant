#[cfg(feature = "hi-doc")]
use hi_doc::{Formatting, SnippetBuilder, Text};
#[cfg(feature = "tree-sitter-highlight")]
use tree_sitter_highlight::HighlightConfiguration;

use crate::span::SimpleSpan;

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Severity {
	Warning,
	Error,
}

#[derive(Clone)]
pub struct ReportPart {
	pub msg: String,
	pub severity: Severity,
	pub annotations: Vec<Annotation>,
}

#[derive(Clone)]
pub struct Annotation {
	pub span: SimpleSpan,
	pub msg: String,
}

#[derive(Default, Clone)]
pub struct Report {
	pub parts: Vec<ReportPart>,
}

#[cfg(feature = "tree-sitter-highlight")]
fn highlight(b: &mut SnippetBuilder) {
	let language = tree_sitter_immigrant::LANGUAGE;
	let mut config = HighlightConfiguration::new(
		language.into(),
		"immigrant",
		tree_sitter_immigrant::HIGHLIGHTS_QUERY,
		tree_sitter_immigrant::INJECTIONS_QUERY,
		tree_sitter_immigrant::LOCALS_QUERY,
	)
	.expect("highlight configuration is valid");
	config.configure(&[
		"punctuation.bracket",
		"keyword",
		"property",
		"type",
	]);
	b.highlight(config, |name, _str| {
		match name {
			1 => Formatting::rgb([255, 50, 50]),
			2 => Formatting::rgb([50, 150, 50]),
			3 => Formatting::rgb([120, 150, 50]),
			_ => Formatting::listchar(),
		}
	});
}

impl Report {
	pub fn new() -> Self {
		Self::default()
	}
	#[cfg(feature = "hi-doc")]
	pub fn to_hi_doc(self, src: &str) -> Vec<hi_doc::Source> {
		let mut out = Vec::new();

		let mut snippet_src = src.to_owned();
		// For missing something at the end of input - create real character to point at
		snippet_src.push(' ');

		for part in self.parts {
			let mut builder = SnippetBuilder::new(&snippet_src);
			#[cfg(feature = "tree-sitter-highlight")]
			highlight(&mut builder);
			// TODO: other severity
			for ele in part.annotations {
				let mut ann = builder.error(Text::fragment(
					format!("{}: {}", part.msg, ele.msg),
					Formatting::rgb([127, 127, 255]),
				));
				if ele.span.start == ele.span.end {
					// FIXME: They shouldn't be inclusive by default
					ann = ann.range(ele.span.start as usize..=ele.span.end as usize);
				} else {
					ann = ann.range(ele.span.start as usize..=ele.span.end as usize - 1);
				}
				ann.build();
			}
			out.push(builder.build())
		}
		out
	}
	pub fn is_error(&self) -> bool {
		self.parts.iter().any(|p| p.severity == Severity::Error)
	}

	pub fn error(&mut self, msg: impl AsRef<str>) -> PartBuilder<'_> {
		let part = ReportPart {
			msg: msg.as_ref().to_owned(),
			severity: Severity::Error,
			annotations: vec![],
		};
		self.parts.push(part);
		PartBuilder {
			part: self.parts.last_mut().expect("just inserted"),
		}
	}
}

pub struct PartBuilder<'r> {
	part: &'r mut ReportPart,
}
impl PartBuilder<'_> {
	pub fn annotate(&mut self, msg: impl AsRef<str>, span: SimpleSpan) -> &mut Self {
		self.part.annotations.push(Annotation {
			span,
			msg: msg.as_ref().to_owned(),
		});
		self
	}
}

#[test]
#[cfg(feature = "hi-doc")]
fn diagnostics() {
	use crate::process::NamingConvention;
	use crate::root::SchemaProcessOptions;
	use crate::uid::RenameMap;

	let mut rn = RenameMap::new();

	let mut report = Report::new();

	let src = r#"
			scalar idd = "INTEGER";
			table A {
				idd;
			};
			table A {
				idd;
			};
		"#;
	crate::parser::parse(
		src,
		false,
		&SchemaProcessOptions {
			generator_supports_domain: true,
			naming_convention: NamingConvention::Postgres,
		},
		&mut rn,
		&mut report,
	)
	.expect("parsed");

	assert!(report.is_error());
	let hidoc = report.to_hi_doc(src);
	for hidoc in hidoc {
		let ansi = hi_doc::source_to_ansi(&hidoc);
		println!("{ansi}")
	}
}
