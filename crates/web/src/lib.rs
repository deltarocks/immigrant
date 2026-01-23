use generator_postgres::Pg;
use schema::diagnostics::Report;
use schema::root::SchemaProcessOptions;
use schema::uid::RenameMap;
use wasm_bindgen::prelude::wasm_bindgen;

#[wasm_bindgen]
#[derive(Clone, Copy)]
pub enum ReportSeverity {
	Warn,
	Error,
}

#[derive(Clone)]
#[wasm_bindgen(getter_with_clone)]
pub struct ReportLine {
	pub from: u32,
	pub to: u32,
	pub description: String,
	pub severity: ReportSeverity,
}

#[wasm_bindgen(getter_with_clone)]
pub struct DiffResult {
	pub errors_a: Vec<ReportLine>,
	pub errors_b: Vec<ReportLine>,
	pub diff_up: Option<String>,
	pub diff_down: Option<String>,
}

fn process_report(report: Report) -> Vec<ReportLine> {
	report
		.parts
		.into_iter()
		.map(|p| ReportLine {
			from: p.annotations.first().expect("first").span.start,
			to: p.annotations.first().expect("first").span.end,
			description: p.msg,
			severity: match p.severity {
				schema::diagnostics::Severity::Warning => ReportSeverity::Warn,
				schema::diagnostics::Severity::Error => ReportSeverity::Error,
			},
		})
		.collect()
}

#[wasm_bindgen]
pub fn diff(a: String, b: String) -> DiffResult {
	console_error_panic_hook::set_once();
	let mut report_a = Report::new();
	let mut rn = RenameMap::new();
	let a = schema::parser::parse(
		&a,
		false,
		&SchemaProcessOptions {
			generator_supports_domain: true,
			naming_convention: schema::process::NamingConvention::Postgres,
		},
		&mut rn,
		&mut report_a,
	);
	let mut report_b = Report::new();
	let b = schema::parser::parse(
		&b,
		false,
		&SchemaProcessOptions {
			generator_supports_domain: true,
			naming_convention: schema::process::NamingConvention::Postgres,
		},
		&mut rn,
		&mut report_b,
	);

	let (diff_up, diff_down) = match (a, b) {
		(Ok(a), Ok(b)) => {
			let mut diff_up = String::new();
			let mut diff_down = String::new();
			Pg(&b).diff(
				&Pg(&a),
				&mut diff_up,
				&mut rn.clone(),
				&mut report_a,
				&mut report_b,
			);
			Pg(&a).diff(
				&Pg(&b),
				&mut diff_down,
				&mut rn.clone(),
				&mut report_b,
				&mut report_a,
			);
			(Some(diff_up), Some(diff_down))
		}
		_ => (None, None),
	};

	DiffResult {
		errors_a: process_report(report_a),
		errors_b: process_report(report_b),
		diff_up,
		diff_down,
	}
}

#[test]
fn test() {
	diff(
		r#"
scalar int = sql"INTEGER" @check(t);

table Example {
	a: int;
	b: int;
};
		"#
		.to_owned(),
		r#"
scalar int = sql"INTEGER" @check(t);
scalar int
"#
		.to_owned(),
	);
}
