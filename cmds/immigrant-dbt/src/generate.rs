use std::collections::BTreeMap;

use schema::{
	HasIdent, SchemaTable, SchemaType,
	annotation::{AnnotationList, AnnotationValue, Tracker, UsedFields},
	diagnostics::Report,
	names::ColumnIdent,
	root::Schema,
	sql::{Sql, SqlOp},
	table::Cardinality,
	uid::{RenameExt, RenameMap},
};

use crate::yaml::Yaml;

pub const TRACKED_ANNOTATIONS: [&str; 5] =
	["dbt", "dbt_column", "dbt_from", "dbt_join", "dbt_filter"];

pub struct Options {
	pub project: String,
	pub source: String,
	pub schema: String,
}

pub struct Column {
	pub name: String,
	pub label: Option<String>,
	pub description: String,
	pub data_type: String,
	pub hidden: bool,
	pub sql: Option<String>,
	pub tests: Vec<Yaml>,
}

pub struct Join {
	pub model: String,
	pub alias: Option<String>,
	pub sql_on: String,
	pub relationship: String,
	pub hidden: bool,
}

pub struct Filter {
	pub dimension: String,
	pub value: String,
	pub required: bool,
}

pub struct Model {
	pub name: String,
	pub label: Option<String>,
	pub description: String,
	pub primary_key: Vec<String>,
	pub columns: Vec<Column>,
	pub from_sql: Vec<String>,
	pub joins: Vec<Join>,
	pub filters: Vec<Filter>,
}

struct JoinSpec {
	model: String,
	via: String,
	pairs: Vec<(String, String)>,
	relationship: &'static str,
	reverse: bool,
}

type Pragma<'a> = BTreeMap<&'a str, Option<&'a str>>;

fn label(annotations: &AnnotationList, track: &mut UsedFields) -> Option<String> {
	annotations
		.try_get_single::<Option<String>>("dbt", "label", track)
		.expect("dbt(label) is a string")
}
fn skipped(table: SchemaTable<'_>, track: &mut Tracker) -> bool {
	let track = track.table_(table);
	table.annotations.get_flag("dbt", "skip", track)
}
fn pragmas<'a>(
	annotations: &'a AnnotationList,
	name: &str,
	keys: Option<&[&str]>,
	track: &mut UsedFields,
) -> Vec<Pragma<'a>> {
	annotations
		.annotations
		.iter()
		.filter(|a| a.name == name)
		.map(|a| {
			a.fields
				.iter()
				.filter(|f| keys.is_none_or(|keys| keys.contains(&f.key.as_str())))
				.filter_map(|f| {
					track.used(&a.name, &f.key);
					match &f.value {
						AnnotationValue::String(s) => Some((f.key.as_str(), Some(s.as_str()))),
						AnnotationValue::Set => Some((f.key.as_str(), None)),
						AnnotationValue::Unset => None,
					}
				})
				.collect()
		})
		.collect()
}
fn required<'a>(pragma: &Pragma<'a>, name: &str, key: &str) -> &'a str {
	pragma
		.get(key)
		.copied()
		.flatten()
		.unwrap_or_else(|| panic!("#{name} requires {key} = \"...\""))
}
fn optional<'a>(pragma: &Pragma<'a>, key: &str) -> Option<&'a str> {
	pragma.get(key).copied().flatten()
}

fn expand_refs(sql: &str, opts: &Options) -> String {
	let mut out = String::new();
	let mut rest = sql;
	while let Some(start) = rest.find('{') {
		let Some(len) = rest[start..].find('}') else {
			break;
		};
		let inner = &rest[start + 1..start + len];
		let expanded = match inner.split_once(':') {
			Some(("ref", name)) => Some(format!("{{{{ ref('{name}') }}}}")),
			Some(("source", name)) => {
				Some(format!("{{{{ source('{}', '{name}') }}}}", opts.source))
			}
			_ => None,
		};
		out.push_str(&rest[..start]);
		match expanded {
			Some(expanded) => {
				out.push_str(&expanded);
				rest = &rest[start + len + 1..];
			}
			None => {
				out.push('{');
				rest = &rest[start + 1..];
			}
		}
	}
	out.push_str(rest);
	out
}

fn docs_to_string(mut docs: Vec<String>) -> String {
	if docs.iter().all(|v| v.starts_with(' ')) {
		for ele in docs.iter_mut() {
			ele.remove(0);
			*ele = ele.trim_end().to_owned();
		}
	}
	while matches!(docs.first(), Some(v) if v.is_empty()) {
		docs.remove(0);
	}
	while matches!(docs.last(), Some(v) if v.is_empty()) {
		docs.pop();
	}
	docs.join("\n")
}

pub fn dimension_type(sql_type: &str) -> &'static str {
	let upper = sql_type.trim().to_ascii_uppercase();
	let base = upper.split(['(', ' ', '[']).next().unwrap_or("");
	match base {
		"BOOL" | "BOOLEAN" => "boolean",
		"DATE" => "date",
		"TIMESTAMP" | "TIMESTAMPTZ" => "timestamp",
		"SMALLINT" | "INT" | "INTEGER" | "BIGINT" | "INT2" | "INT4" | "INT8" | "SMALLSERIAL"
		| "SERIAL" | "BIGSERIAL" | "NUMERIC" | "DECIMAL" | "REAL" | "FLOAT" | "FLOAT4"
		| "FLOAT8" | "DOUBLE" => "number",
		_ => "string",
	}
}

fn relationship(from: Cardinality, to: Cardinality) -> &'static str {
	match (from, to) {
		(Cardinality::One, Cardinality::One) => "one-to-one",
		(Cardinality::Many, Cardinality::One) => "many-to-one",
		(Cardinality::One, Cardinality::Many) => "one-to-many",
		(Cardinality::Many, Cardinality::Many) => "many-to-many",
	}
}

fn conjuncts<'a>(sql: &'a Sql, out: &mut Vec<&'a Sql>) {
	match sql {
		Sql::BinOp(a, SqlOp::And, b) => {
			conjuncts(a, out);
			conjuncts(b, out);
		}
		Sql::Parened(inner) => conjuncts(inner, out),
		other => out.push(other),
	}
}
fn equality_values(sql: &Sql, column: ColumnIdent, out: &mut Vec<String>) -> bool {
	match sql {
		Sql::Parened(inner) => equality_values(inner, column, out),
		Sql::BinOp(a, SqlOp::Or, b) => {
			equality_values(a, column, out) && equality_values(b, column, out)
		}
		Sql::BinOp(a, SqlOp::Eq, b) => match (&**a, &**b) {
			(Sql::Ident(c), Sql::String(v)) | (Sql::String(v), Sql::Ident(c)) if *c == column => {
				out.push(v.clone());
				true
			}
			_ => false,
		},
		_ => false,
	}
}
fn check_values(table: &SchemaTable<'_>, column: ColumnIdent) -> Option<Vec<String>> {
	for check in table.checks() {
		let mut parts = Vec::new();
		conjuncts(&check.check, &mut parts);
		for part in parts {
			let mut values = Vec::new();
			if equality_values(part, column, &mut values) && !values.is_empty() {
				return Some(values);
			}
		}
	}
	None
}

fn is_unique_single(table: &SchemaTable<'_>, column: ColumnIdent) -> bool {
	let single = |columns: &[ColumnIdent]| columns == [column];
	table.pk().is_some_and(|pk| single(&pk.columns))
		|| table.unique_constraints().any(|u| single(&u.columns))
		|| table
			.indexes()
			.any(|i| i.unique && single(&i.field_idents().into_iter().collect::<Vec<_>>()))
}

fn test(name: &str, arguments: Yaml) -> Yaml {
	Yaml::map().with(name, Yaml::map().with("arguments", arguments))
}

fn build_columns(
	table: &SchemaTable<'_>,
	rn: &RenameMap,
	report: &mut Report,
	opts: &Options,
	track: &mut Tracker,
) -> Vec<Column> {
	let mut columns = Vec::new();
	for column in table.columns() {
		let track_column = track.column(column);
		if column.annotations.get_flag("dbt", "skip", track_column) {
			continue;
		}
		let ty = column.ty();
		let track_ty = track.ty(ty);
		let (data_type, enum_values) = match ty {
			SchemaType::Scalar(s) => {
				if s.annotations.get_flag("dbt", "skip", track_ty) {
					continue;
				}
				(s.inner_type(rn, report).raw().to_string(), None)
			}
			SchemaType::Enum(e) => (
				e.db_type(rn).raw().to_string(),
				Some(
					e.db_items(rn)
						.iter()
						.map(|i| i.raw().to_string())
						.collect::<Vec<_>>(),
				),
			),
			SchemaType::Composite(_) => (column.db_type(rn, report).raw().to_string(), None),
		};
		let values = enum_values.or_else(|| check_values(table, column.id()));
		let mut description = docs_to_string(column.docs.clone());
		if let Some(values) = &values {
			if !description.is_empty() {
				description.push('\n');
			}
			description.push_str("One of: ");
			description.push_str(&values.join(", "));
		}

		let mut tests = Vec::new();
		if !column.nullable {
			tests.push(Yaml::str("not_null"));
		}
		if is_unique_single(table, column.id()) {
			tests.push(Yaml::str("unique"));
		}
		if let Some(values) = &values {
			tests.push(test(
				"accepted_values",
				Yaml::map().with("values", Yaml::strs(values.iter().map(String::as_str))),
			));
		}
		for fk in table.foreign_keys() {
			let source = fk.source_columns();
			if source.len() != 1 || source[0] != column.id() {
				continue;
			}
			let target = fk.target_table();
			if skipped(target, track) {
				continue;
			}
			let target_column = fk.target_db_columns(rn);
			tests.push(test(
				"relationships",
				Yaml::map()
					.with("to", Yaml::str(format!("ref('{}')", target.db(rn).raw())))
					.with("field", Yaml::str(target_column[0].raw())),
			));
		}

		let track_column = track.column(column);
		columns.push(Column {
			name: column.db(rn).raw().to_string(),
			label: label(&column.annotations, track_column),
			description,
			data_type,
			hidden: column.annotations.get_flag("dbt", "hidden", track_column),
			sql: None,
			tests,
		});
	}
	let track_table = track.table_(*table);
	for pragma in pragmas(
		&table.annotations,
		"dbt_column",
		Some(&["name", "type", "sql", "description", "label", "hidden"]),
		track_table,
	) {
		columns.push(Column {
			name: required(&pragma, "dbt_column", "name").to_owned(),
			label: optional(&pragma, "label").map(str::to_owned),
			description: optional(&pragma, "description").unwrap_or("").to_owned(),
			data_type: required(&pragma, "dbt_column", "type").to_owned(),
			hidden: pragma.contains_key("hidden"),
			sql: Some(expand_refs(required(&pragma, "dbt_column", "sql"), opts)),
			tests: Vec::new(),
		});
	}
	columns
}

fn join_specs(
	schema: &Schema,
	table: &SchemaTable<'_>,
	rn: &RenameMap,
	track: &mut Tracker,
) -> Vec<JoinSpec> {
	let mut specs = Vec::new();
	for fk in table.foreign_keys() {
		let target = fk.target_table();
		if skipped(target, track) {
			continue;
		}
		let source_columns = fk.source_db_columns(rn);
		let target_columns = fk.target_db_columns(rn);
		let (from, to) = fk.cardinality();
		specs.push(JoinSpec {
			model: target.db(rn).raw().to_string(),
			via: source_columns[0].raw().to_string(),
			pairs: source_columns
				.iter()
				.zip(target_columns.iter())
				.map(|(a, b)| (a.raw().to_string(), b.raw().to_string()))
				.collect(),
			relationship: relationship(from, to),
			reverse: false,
		});
	}
	for other in schema.tables() {
		let other = SchemaTable {
			schema,
			table: other,
		};
		if skipped(other, track) {
			continue;
		}
		for fk in other.foreign_keys() {
			if fk.target_table().id() != table.id() {
				continue;
			}
			let source_columns = fk.source_db_columns(rn);
			let target_columns = fk.target_db_columns(rn);
			let (from, to) = fk.cardinality();
			specs.push(JoinSpec {
				model: other.db(rn).raw().to_string(),
				via: source_columns[0].raw().to_string(),
				pairs: target_columns
					.iter()
					.zip(source_columns.iter())
					.map(|(a, b)| (a.raw().to_string(), b.raw().to_string()))
					.collect(),
				relationship: relationship(to, from),
				reverse: true,
			});
		}
	}
	specs
}

fn build_joins(
	schema: &Schema,
	table: &SchemaTable<'_>,
	rn: &RenameMap,
	track: &mut Tracker,
) -> Vec<Join> {
	let this = table.db(rn).raw().to_string();
	let specs = join_specs(schema, table, rn, track);
	let mut counts = BTreeMap::<&str, usize>::new();
	for spec in &specs {
		*counts.entry(spec.model.as_str()).or_default() += 1;
	}
	let mut joins: Vec<Join> = specs
		.iter()
		.map(|spec| {
			let alias = if spec.model == this || counts[spec.model.as_str()] > 1 {
				let suffix = if spec.reverse { "by" } else { "via" };
				Some(format!("{}_{suffix}_{}", spec.model, spec.via))
			} else {
				None
			};
			let target = alias.as_deref().unwrap_or(&spec.model);
			let sql_on = spec
				.pairs
				.iter()
				.map(|(a, b)| format!("${{{this}.{a}}} = ${{{target}.{b}}}"))
				.collect::<Vec<_>>()
				.join(" AND ");
			Join {
				model: spec.model.clone(),
				alias,
				sql_on,
				relationship: spec.relationship.to_owned(),
				hidden: false,
			}
		})
		.collect();
	let track_table = track.table_(*table);
	for pragma in pragmas(
		&table.annotations,
		"dbt_join",
		Some(&["model", "sql_on", "relationship", "alias", "hidden"]),
		track_table,
	) {
		joins.push(Join {
			model: required(&pragma, "dbt_join", "model").to_owned(),
			alias: optional(&pragma, "alias").map(str::to_owned),
			sql_on: required(&pragma, "dbt_join", "sql_on").to_owned(),
			relationship: required(&pragma, "dbt_join", "relationship").to_owned(),
			hidden: pragma.contains_key("hidden"),
		});
	}
	joins
}

fn build_filters(table: &SchemaTable<'_>, track: &mut UsedFields) -> Vec<Filter> {
	pragmas(&table.annotations, "dbt_filter", None, track)
		.iter()
		.map(|pragma| {
			let mut filters =
				pragma
					.iter()
					.filter(|(key, _)| **key != "required")
					.map(|(key, value)| Filter {
						dimension: (*key).to_owned(),
						value: value
							.expect("#dbt_filter(dimension = \"value\")")
							.to_owned(),
						required: pragma.contains_key("required"),
					});
			let filter = filters.next().expect("#dbt_filter needs a dimension");
			assert!(
				filters.next().is_none(),
				"#dbt_filter takes one dimension per pragma"
			);
			filter
		})
		.collect()
}

pub fn build(
	schema: &Schema,
	rn: &RenameMap,
	report: &mut Report,
	opts: &Options,
	track: &mut Tracker,
) -> Vec<Model> {
	let mut models = Vec::new();
	for table in schema.tables() {
		let table = SchemaTable { schema, table };
		if skipped(table, track) {
			continue;
		}
		let columns = build_columns(&table, rn, report, opts, track);
		if columns.is_empty() {
			continue;
		}
		let primary_key = table
			.pk()
			.map(|pk| {
				pk.columns
					.iter()
					.map(|c| table.db_name(c, rn).raw().to_string())
					.collect()
			})
			.unwrap_or_default();
		let joins = build_joins(schema, &table, rn, track);
		let track_table = track.table_(table);
		let from_sql = pragmas(&table.annotations, "dbt_from", Some(&["sql"]), track_table)
			.iter()
			.map(|p| expand_refs(required(p, "dbt_from", "sql"), opts))
			.collect();
		models.push(Model {
			name: table.db(rn).raw().to_string(),
			label: label(&table.annotations, track_table),
			description: docs_to_string(table.docs.clone()),
			primary_key,
			columns,
			from_sql,
			joins,
			filters: build_filters(&table, track_table),
		});
	}
	models
}

fn quote_ident(name: &str) -> String {
	format!("\"{}\"", name.replace('"', "\"\""))
}

pub fn model_sql(model: &Model, opts: &Options) -> String {
	let columns = model
		.columns
		.iter()
		.map(|c| match &c.sql {
			Some(sql) => format!("\t{sql} as {}", quote_ident(&c.name)),
			None => format!("\tt.{}", quote_ident(&c.name)),
		})
		.collect::<Vec<_>>()
		.join(",\n");
	let mut out = format!(
		"select\n{columns}\nfrom {{{{ source('{}', '{}') }}}} t\n",
		opts.source, model.name
	);
	for from in &model.from_sql {
		out.push_str(from);
		out.push('\n');
	}
	out
}

fn model_yaml(model: &Model) -> Yaml {
	let mut meta = Yaml::map();
	if let Some(label) = &model.label {
		meta.push("label", Yaml::str(label));
	}
	match model.primary_key.as_slice() {
		[] => {}
		[single] => meta.push("primary_key", Yaml::str(single)),
		many => meta.push("primary_key", Yaml::strs(many.iter().map(String::as_str))),
	}
	if !model.filters.is_empty() {
		meta.push(
			"default_filters",
			Yaml::Seq(
				model
					.filters
					.iter()
					.map(|f| {
						let mut y = Yaml::map().with(&f.dimension, Yaml::str(&f.value));
						if f.required {
							y.push("required", Yaml::Bool(true));
						}
						y
					})
					.collect(),
			),
		);
	}
	let count_column = model.primary_key.first().unwrap_or(&model.columns[0].name);
	meta.push(
		"metrics",
		Yaml::map().with(
			"count",
			Yaml::map()
				.with("type", Yaml::str("count"))
				.with("sql", Yaml::str(format!("${{TABLE}}.{count_column}"))),
		),
	);
	if !model.joins.is_empty() {
		meta.push(
			"joins",
			Yaml::Seq(
				model
					.joins
					.iter()
					.map(|join| {
						let mut y = Yaml::map().with("join", Yaml::str(&join.model));
						if let Some(alias) = &join.alias {
							y.push("alias", Yaml::str(alias));
						}
						y.push("type", Yaml::str("left"));
						y.push("relationship", Yaml::str(&join.relationship));
						y.push("sql_on", Yaml::str(&join.sql_on));
						if join.hidden {
							y.push("hidden", Yaml::Bool(true));
						}
						y
					})
					.collect(),
			),
		);
	}
	let columns = model
		.columns
		.iter()
		.map(|column| {
			let mut dimension =
				Yaml::map().with("type", Yaml::str(dimension_type(&column.data_type)));
			if let Some(label) = &column.label {
				dimension.push("label", Yaml::str(label));
			}
			if column.hidden {
				dimension.push("hidden", Yaml::Bool(true));
			}
			let mut y = Yaml::map()
				.with("name", Yaml::str(&column.name))
				.with("description", Yaml::str(&column.description))
				.with("data_type", Yaml::str(&column.data_type))
				.with(
					"config",
					Yaml::map().with("meta", Yaml::map().with("dimension", dimension)),
				);
			if !column.tests.is_empty() {
				y.push(
					"data_tests",
					Yaml::Seq(column.tests.iter().map(clone_yaml).collect()),
				);
			}
			y
		})
		.collect();
	Yaml::map()
		.with("name", Yaml::str(&model.name))
		.with("description", Yaml::str(&model.description))
		.with("config", Yaml::map().with("meta", meta))
		.with("columns", Yaml::Seq(columns))
}

fn clone_yaml(v: &Yaml) -> Yaml {
	match v {
		Yaml::Str(s) => Yaml::Str(s.clone()),
		Yaml::Int(i) => Yaml::Int(*i),
		Yaml::Bool(b) => Yaml::Bool(*b),
		Yaml::Seq(items) => Yaml::Seq(items.iter().map(clone_yaml).collect()),
		Yaml::Map(entries) => Yaml::Map(
			entries
				.iter()
				.map(|(k, v)| (k.clone(), clone_yaml(v)))
				.collect(),
		),
	}
}

pub fn schema_yaml(models: &[Model]) -> String {
	Yaml::map()
		.with("version", Yaml::Int(2))
		.with("models", Yaml::Seq(models.iter().map(model_yaml).collect()))
		.render()
}

pub fn sources_yaml(models: &[Model], opts: &Options) -> String {
	let tables = models
		.iter()
		.map(|m| {
			Yaml::map()
				.with("name", Yaml::str(&m.name))
				.with("description", Yaml::str(&m.description))
		})
		.collect();
	Yaml::map()
		.with("version", Yaml::Int(2))
		.with(
			"sources",
			Yaml::Seq(vec![
				Yaml::map()
					.with("name", Yaml::str(&opts.source))
					.with("schema", Yaml::str(&opts.schema))
					.with("tables", Yaml::Seq(tables)),
			]),
		)
		.render()
}

pub fn project_yaml(opts: &Options) -> String {
	Yaml::map()
		.with("name", Yaml::str(&opts.project))
		.with("version", Yaml::str("1.0.0"))
		.with("config-version", Yaml::Int(2))
		.with("profile", Yaml::str(&opts.project))
		.with("model-paths", Yaml::strs(["models"]))
		.with(
			"models",
			Yaml::map().with(
				&opts.project,
				Yaml::map().with("+materialized", Yaml::str("view")),
			),
		)
		.render()
}

pub fn sanitize_name(name: &str) -> String {
	let mut out: String = name
		.chars()
		.map(|c| {
			let c = c.to_ascii_lowercase();
			if c.is_ascii_alphanumeric() { c } else { '_' }
		})
		.collect();
	if out.chars().next().is_none_or(|c| !c.is_ascii_alphabetic()) {
		out.insert(0, '_');
	}
	out
}
