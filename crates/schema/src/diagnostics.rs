#[cfg(feature = "hi-doc")]
use hi_doc::{Formatting, SnippetBuilder, Text};

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

impl Report {
	pub fn new() -> Self {
		Self::default()
	}
	#[cfg(feature = "hi-doc")]
	pub fn to_hi_doc(self, src: &str) -> Vec<hi_doc::Source> {
		let mut out = Vec::new();

		for part in self.parts {
			let mut builder = SnippetBuilder::new(src);
			// TODO: other severity
			for ele in part.annotations {
				let mut ann =
					builder.error(Text::fragment(format!("{}: {}", part.msg, ele.msg), Formatting::rgb([127, 127, 255])));
				dbg!(&src);
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

	pub fn error(&mut self, msg: impl AsRef<str>) -> PartBuilder {
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
