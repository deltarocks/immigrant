import { assert, assertEquals } from "@std/assert";
import { diff, diff_project, ReportSeverity } from "./mod.ts";

Deno.test("diff generates migration sql", () => {
	const result = diff(
		"",
		'scalar i32 = sql"INTEGER";\ntable A {\n\ti32;\n};\n',
	);
	assertEquals(result.errors_a.length, 0);
	assertEquals(result.errors_b.length, 0);
	assert(result.diff_up?.includes("CREATE TABLE"));
	assert(result.diff_down?.includes("DROP TABLE"));
});

Deno.test("diff reports errors with spans", () => {
	const result = diff("", "table A {};\n");
	assert(result.errors_b.length > 0);
	assertEquals(result.errors_b[0].severity, ReportSeverity.Error);
	assertEquals(result.errors_b[0].file, "db.schema");
});

const roots = ["@main/db.schema", "@extra/db.schema"];
const project = {
	"@main/db.schema":
		'import { id } from "./types.schema";\ntable Alpha {\n\tid;\n};\n',
	"@main/types.schema": 'scalar id = sql"INTEGER";\n',
	"@extra/db.schema":
		'import { id } from "@main/types.schema";\ntable Beta {\n\tid;\n};\n',
};

Deno.test("diff_project resolves imports across plugins", () => {
	const result = diff_project([], {}, roots, project);
	assertEquals(result.errors_a.length, 0);
	assertEquals(result.errors_b.length, 0);
	assert(result.diff_up?.includes("CREATE DOMAIN id"));
	assert(result.diff_up?.includes("CREATE TABLE betas"));
	assert(result.diff_down?.includes("DROP DOMAIN id"));
});

Deno.test("diff_project drops tables of removed roots", () => {
	const result = diff_project(roots, project, ["@main/db.schema"], project);
	assertEquals(result.diff_up?.trim(), "DROP TABLE betas;");
});

Deno.test("diff_project reports the file of an error", () => {
	const result = diff_project([], {}, roots, {
		...project,
		"@main/types.schema": "scalar id = ;\n",
	});
	assert(result.errors_b.length > 0);
	assertEquals(result.errors_b[0].file, "@main/types.schema");
});
