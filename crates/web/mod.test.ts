import { assert, assertEquals } from "@std/assert";
import { diff, ReportSeverity } from "./mod.ts";

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
});
