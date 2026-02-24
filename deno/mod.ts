import { assert } from "@std/assert";

function pruned(v: (string | undefined)[]): string[] {
	return v.filter((v) => v !== undefined);
}
function indented(v: string[]): string[] {
	return v.map((v) => "\t" + v);
}
function optional(b: boolean, v: string): string | undefined {
	if (b) return v;
	return undefined;
}
function optionStr(b: boolean, v: string): string {
	if (b) return v;
	return "";
}
function spStr(s: string | undefined, ch: string = " "): string {
	if (s) return ch + s;
	return "";
}

export abstract class Item {
	#dbName?: string;
	constructor(public name: string) {}

	with(cb: (item: this) => void): this {
		cb(this);
		return this;
	}

	dbName(dbName: string): this {
		assert(!this.#dbName, "dbName is already set");
		this.#dbName = dbName;
		return this;
	}

	abstract output(): string[];
}

abstract class SchemaItem extends Item {
}
abstract class TableItem extends Item {
}

type OnDelete = "cascade";
class TableColumn extends TableItem {
	constructor(public table: Table, name: string, public ty: string = name) {
		super(name);
	}
	#primaryKey: boolean = false;
	primaryKey(): this {
		this.#primaryKey = true;
		return this;
	}
	#optional: boolean = false;
	optional(): this {
		this.#optional = true;
		return this;
	}

	fks: { col: TableColumn; onDelete?: OnDelete }[] = [];
	fk(to: TableColumn, onDelete?: OnDelete): this {
		this.fks.push({ col: to, onDelete });
		return this;
	}
	output() {
		const v = this.name === this.ty
			? `${this.name}${optionStr(this.#optional, "?")}`
			: `${this.name}${optionStr(this.#optional, "?")}: ${this.ty}`;
		return [
			v + optionStr(this.#primaryKey, " @primary_key") +
			this.fks.map((v) =>
				` ~${spStr(v.onDelete, ".")} ${v.col.table.name} (${v.col.name})`
			).join("") + ";",
		];
	}
}

export class Scalar extends SchemaItem {
	constructor(name: string, public sql: string) {
		super(name);
	}
	#inline: boolean = false;
	inline(): this {
		this.#inline = true;
		return this;
	}
	#default?: string;
	default(expr: string) {
		assert(!this.#default, "default is already set");
		this.#default = expr;
		return this;
	}
	output() {
		return pruned([
			optional(this.#inline, "@inline"),
			`scalar ${this.name} = sql"${this.sql}"${spStr(this.#default)};`,
		]);
	}
}

export class Table extends SchemaItem {
	items: TableItem[] = [];

	column(name: string, ty?: Scalar) {
		const existing = this.items.find((i) =>
			i instanceof TableColumn && i.name === name
		) as TableColumn | undefined;

		if (existing) {
			// TODO: Relax requirement, check set type instead
			assert(
				!ty,
				"column already exists, type can only be specified for new columns",
			);
			return existing;
		}
		assert(ty, "column doesn't exists and requires type");
		const column = new TableColumn(this, name, ty?.name);
		this.items.push(column);
		return column;
	}

	output() {
		return [
			`table ${this.name} {`,
			...indented(this.items.flatMap((v) => v.output())),
			"};",
		];
	}
}

export class Schema {
	items: SchemaItem[] = [];
	table(name: string): Table {
		const table = new Table(name);
		this.items.push(table);
		return table;
	}
	scalar(name: string, sql: string) {
		const existing = this.items.find((v) =>
			v instanceof Scalar && v.name === name
		) as Scalar | undefined;
		if (existing) {
			assert(existing.sql === sql, "mismatching scalar definitions");
			return existing;
		}
		const scalar = new Scalar(name, sql);
		this.items.push(scalar);
		return scalar;
	}

	toString() {
		this.items.sort((a, b) => {
			if (a instanceof Scalar && !(b instanceof Scalar)) return -1;
			return 0;
		});
		return this.items.flatMap((v) => v.output()).join("\n");
	}
}
