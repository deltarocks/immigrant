use schema::{
	column::ColumnAttribute,
	root::Schema,
	scalar::ScalarAttribute,
	table::TableAttribute,
	uid::{RenameExt, RenameMap},
};

/// Can be updated in database source code, and some are already doing that,
/// but for simplicity assuming default here.
pub const MAX_IDENTIFIER_LEN: usize = 63;

fn validate_db<T: RenameExt>(v: &T, rn: &RenameMap) {
	let str = v.db(rn);
	let str = str.raw();
	assert!(
		str.len() <= MAX_IDENTIFIER_LEN,
		"{str} is larger than max allowed identifier! consider renaming"
	);
}

pub fn validate(_code: &str, schema: &Schema, rn: &RenameMap) {
	for ele in schema.items() {
		validate_db(&ele, rn);
		match ele {
			schema::SchemaItem::Table(t) => {
				for ele in t.columns() {
					validate_db(&ele, rn);
					for ele in &ele.attributes {
						match ele {
							ColumnAttribute::Check(_)
							| ColumnAttribute::Unique(_)
							| ColumnAttribute::PrimaryKey(_)
							| ColumnAttribute::Index(_) => panic!("should be propagated"),
							ColumnAttribute::Default(_) | ColumnAttribute::InitializeAs(_) => {}
						}
					}
				}
				for ele in &t.attributes {
					match ele {
						TableAttribute::Check(c) => validate_db(c, rn),
						TableAttribute::Unique(u) => validate_db(u, rn),
						TableAttribute::PrimaryKey(p) => validate_db(p, rn),
						TableAttribute::Index(i) => validate_db(i, rn),
						TableAttribute::External => {}
					}
				}
			}
			schema::SchemaItem::Enum(e) => {
				for ele in &e.items {
					validate_db(ele, rn);
				}
			}
			schema::SchemaItem::Scalar(s) => {
				for ele in &s.attributes {
					match ele {
						ScalarAttribute::Check(c) => validate_db(c, rn),
						ScalarAttribute::PrimaryKey(_)
						| ScalarAttribute::Unique(_)
						| ScalarAttribute::Index(_) => panic!("should be propagated"),
						ScalarAttribute::Default(_)
						| ScalarAttribute::Inline
						| ScalarAttribute::External => {}
					}
				}
			}
			schema::SchemaItem::Composite(c) => {
				for ele in &c.fields {
					validate_db(ele, rn);
				}
			}
			schema::SchemaItem::View(_) => {}
		}
	}
}
