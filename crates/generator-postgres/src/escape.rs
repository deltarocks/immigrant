//! Provides functions for escaping literals and identifiers for use
//! in SQL queries.
//!
//! Prefer parameterized queries where possible. Do not escape
//! parameters in a parameterized query.
//!
//! Taken from https://github.com/sfackler/rust-postgres/blob/e8f44ecf9d7ef749261b8926bf143b6a472768dc/postgres-protocol/src/escape/mod.rs
//! TODO: Try to somehow use already used `pg_query` for this task?

/// Escape a literal and surround result with single quotes. Not
/// recommended in most cases.
///
/// If input contains backslashes, result will be of the form `
/// E'...'` so it is safe to use regardless of the setting of
/// standard_conforming_strings.
// pub fn escape_literal(input: &str) -> String {
// 	escape_internal(input, false)
// }

/// Immigrant addition: keyword list
/// Extracted using gen_keywordlist.pl, all not marked as non-reserved;
/// (Entries marked as non-reserved, but requiring AS/having limitations on context are included in this list)
///
/// Sorted for binary search.
const KWS: [&str; 167] = [
	"ALL",
	"ANALYSE",
	"ANALYZE",
	"AND",
	"ANY",
	"ARRAY",
	"AS",
	"ASC",
	"ASYMMETRIC",
	"AUTHORIZATION",
	"BETWEEN",
	"BIGINT",
	"BINARY",
	"BIT",
	"BOOLEAN",
	"BOTH",
	"CASE",
	"CAST",
	"CHAR",
	"CHARACTER",
	"CHECK",
	"COALESCE",
	"COLLATE",
	"COLLATION",
	"COLUMN",
	"CONCURRENTLY",
	"CONSTRAINT",
	"CREATE",
	"CROSS",
	"CURRENT_CATALOG",
	"CURRENT_DATE",
	"CURRENT_ROLE",
	"CURRENT_SCHEMA",
	"CURRENT_TIME",
	"CURRENT_TIMESTAMP",
	"CURRENT_USER",
	"DAY",
	"DEC",
	"DECIMAL",
	"DEFAULT",
	"DEFERRABLE",
	"DESC",
	"DISTINCT",
	"DO",
	"ELSE",
	"END",
	"EXCEPT",
	"EXISTS",
	"EXTRACT",
	"FALSE",
	"FETCH",
	"FILTER",
	"FLOAT",
	"FOR",
	"FOREIGN",
	"FREEZE",
	"FROM",
	"FULL",
	"GRANT",
	"GREATEST",
	"GROUP",
	"GROUPING",
	"HAVING",
	"HOUR",
	"ILIKE",
	"IN",
	"INITIALLY",
	"INNER",
	"INOUT",
	"INT",
	"INTEGER",
	"INTERSECT",
	"INTERVAL",
	"INTO",
	"IS",
	"ISNULL",
	"JOIN",
	"JSON_ARRAY",
	"JSON_ARRAYAGG",
	"JSON_OBJECT",
	"JSON_OBJECTAGG",
	"LATERAL",
	"LEADING",
	"LEAST",
	"LEFT",
	"LIKE",
	"LIMIT",
	"LOCALTIME",
	"LOCALTIMESTAMP",
	"MINUTE",
	"MONTH",
	"NATIONAL",
	"NATURAL",
	"NCHAR",
	"NONE",
	"NORMALIZE",
	"NOT",
	"NOTNULL",
	"NULL",
	"NULLIF",
	"NUMERIC",
	"OFFSET",
	"ON",
	"ONLY",
	"OR",
	"ORDER",
	"OUT",
	"OUTER",
	"OVER",
	"OVERLAPS",
	"OVERLAY",
	"PLACING",
	"POSITION",
	"PRECISION",
	"PRIMARY",
	"REAL",
	"REFERENCES",
	"RETURNING",
	"RIGHT",
	"ROW",
	"SECOND",
	"SELECT",
	"SESSION_USER",
	"SETOF",
	"SIMILAR",
	"SMALLINT",
	"SOME",
	"SUBSTRING",
	"SYMMETRIC",
	"SYSTEM_USER",
	"TABLE",
	"TABLESAMPLE",
	"THEN",
	"TIME",
	"TIMESTAMP",
	"TO",
	"TRAILING",
	"TREAT",
	"TRIM",
	"TRUE",
	"UNION",
	"UNIQUE",
	"USER",
	"USING",
	"VALUES",
	"VARCHAR",
	"VARIADIC",
	"VARYING",
	"VERBOSE",
	"WHEN",
	"WHERE",
	"WINDOW",
	"WITH",
	"WITHIN",
	"WITHOUT",
	"XMLATTRIBUTES",
	"XMLCONCAT",
	"XMLELEMENT",
	"XMLEXISTS",
	"XMLFOREST",
	"XMLNAMESPACES",
	"XMLPARSE",
	"XMLPI",
	"XMLROOT",
	"XMLSERIALIZE",
	"XMLTABLE",
	"YEAR",
];
/// Immigrant addition: allow to be unescaped
pub fn should_escape(input: &str) -> bool {
	for (i, l) in input.chars().enumerate() {
		if !l.is_ascii() {
			// Any non-ascii character should be escaped
			return true;
		}
		match l {
			// Postgres also allows non-latin letters and letters with diacritical marks...
			// But lets keep them quoted.
			//
			// Any non-lowercase identifier will be folded to lowercase by postgres,
			// should escape if we don't want that.
			'a'..='z' | '_' => {}
			// Not allowed as first letter
			'0'..='9' | '$' if i != 0 => {}
			_ => {
				return true;
			}
		}
	}
	if KWS.binary_search(&input.to_uppercase().as_str()).is_ok() {
		return true;
	}
	false
}

/// Escape an identifier and surround result with double quotes.
pub fn escape_identifier(input: &str) -> String {
	// immigrant: allow some identifiers to be left unescaped
	if !should_escape(input) {
		return input.to_owned();
	}
	// /immigrant
	escape_internal(input, true)
}

// Translation of PostgreSQL libpq's PQescapeInternal(). Does not
// require a connection because input string is known to be valid
// UTF-8.
//
// Escape arbitrary strings.  If as_ident is true, we escape the
// result as an identifier; if false, as a literal.  The result is
// returned in a newly allocated buffer.  If we fail due to an
// encoding violation or out of memory condition, we return NULL,
// storing an error message into conn.
fn escape_internal(input: &str, as_ident: bool) -> String {
	let mut num_backslashes = 0;
	let mut num_quotes = 0;
	let quote_char = if as_ident { '"' } else { '\'' };

	// Scan the string for characters that must be escaped.
	for ch in input.chars() {
		if ch == quote_char {
			num_quotes += 1;
		} else if ch == '\\' {
			num_backslashes += 1;
		}
	}

	// Allocate output String.
	let mut result_size = input.len() + num_quotes + 3; // two quotes, plus a NUL
	if !as_ident && num_backslashes > 0 {
		result_size += num_backslashes + 2;
	}

	let mut output = String::with_capacity(result_size);

	// If we are escaping a literal that contains backslashes, we use
	// the escape string syntax so that the result is correct under
	// either value of standard_conforming_strings.  We also emit a
	// leading space in this case, to guard against the possibility
	// that the result might be interpolated immediately following an
	// identifier.
	if !as_ident && num_backslashes > 0 {
		output.push(' ');
		output.push('E');
	}

	// Opening quote.
	output.push(quote_char);

	// Use fast path if possible.
	//
	// We've already verified that the input string is well-formed in
	// the current encoding.  If it contains no quotes and, in the
	// case of literal-escaping, no backslashes, then we can just copy
	// it directly to the output buffer, adding the necessary quotes.
	//
	// If not, we must rescan the input and process each character
	// individually.
	if num_quotes == 0 && (num_backslashes == 0 || as_ident) {
		output.push_str(input);
	} else {
		for ch in input.chars() {
			if ch == quote_char || (!as_ident && ch == '\\') {
				output.push(ch);
			}
			output.push(ch);
		}
	}

	output.push(quote_char);

	output
}
