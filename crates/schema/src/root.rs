use itertools::Itertools;
use std::{
	collections::{HashMap, HashSet},
	mem,
};

use super::{
	scalar::{Enum, Scalar},
	table::Table,
};
use crate::{
	HasIdent, SchemaComposite, SchemaEnum, SchemaItem, SchemaScalar, SchemaSql, SchemaTable,
	SchemaTableOrView, SchemaType, SchemaView,
	composite::Composite,
	diagnostics::Report,
	ids::Ident,
	mixin::Mixin,
	names::{DbNativeType, DbTable, DbType, TableIdent, TypeIdent},
	process::{NamingConvention, Pgnc, check_unique_identifiers, check_unique_mixin_identifiers},
	scalar::PropagatedScalarData,
	sql::Sql,
	uid::{RenameExt, RenameMap},
	util::UniqueMap as _,
	view::View,
};

#[derive(derivative::Derivative)]
#[derivative(Debug)]
pub enum Item {
	#[derivative(Debug = "transparent")]
	Table(Table),
	#[derivative(Debug = "transparent")]
	Mixin(Mixin),
	#[derivative(Debug = "transparent")]
	Enum(Enum),
	#[derivative(Debug = "transparent")]
	Scalar(Scalar),
	#[derivative(Debug = "transparent")]
	Composite(Composite),
	#[derivative(Debug = "transparent")]
	View(View),
}
impl Item {
	pub fn is_table(&self) -> bool {
		matches!(self, Self::Table(_))
	}
	pub fn as_table(&self) -> Option<&Table> {
		match self {
			Self::Table(value) => Some(value),
			_ => None,
		}
	}
	pub fn as_table_mut(&mut self) -> Option<&mut Table> {
		match self {
			Self::Table(value) => Some(value),
			_ => None,
		}
	}
	pub fn is_enum(&self) -> bool {
		matches!(self, Self::Enum(_))
	}
	pub fn as_enum(&self) -> Option<&Enum> {
		match self {
			Self::Enum(value) => Some(value),
			_ => None,
		}
	}
	pub fn as_enum_mut(&mut self) -> Option<&mut Enum> {
		match self {
			Self::Enum(value) => Some(value),
			_ => None,
		}
	}
	pub fn is_scalar(&self) -> bool {
		matches!(self, Self::Scalar(_))
	}
	pub fn as_scalar(&self) -> Option<&Scalar> {
		match self {
			Self::Scalar(value) => Some(value),
			_ => None,
		}
	}
	pub fn as_composite(&self) -> Option<&Composite> {
		match self {
			Self::Composite(value) => Some(value),
			_ => None,
		}
	}
	pub fn as_view(&self) -> Option<&View> {
		match self {
			Self::View(value) => Some(value),
			_ => None,
		}
	}
	pub fn as_composite_mut(&mut self) -> Option<&mut Composite> {
		match self {
			Self::Composite(value) => Some(value),
			_ => None,
		}
	}
	pub fn as_scalar_mut(&mut self) -> Option<&mut Scalar> {
		match self {
			Self::Scalar(value) => Some(value),
			_ => None,
		}
	}
}

pub struct SchemaProcessOptions {
	/// If not - then every scalar is transformed to a regular type by inlining.
	pub generator_supports_domain: bool,
	/// How to create item names, how attribute deduplication should be handled, etc.
	pub naming_convention: NamingConvention,
}

#[derive(Debug, Default)]
pub struct Schema(pub Vec<Item>);
impl Schema {
	pub fn process(
		&mut self,
		options: &SchemaProcessOptions,
		rn: &mut RenameMap,
		report: &mut Report,
	) {
		self.0.sort_by_key(|i| match i {
			Item::Table(_) => 1,
			Item::Enum(_) => 0,
			Item::Scalar(_) => 9997,
			Item::Composite(_) => 9998,
			Item::View(_) => 9999,
			Item::Mixin(_) => 10000,
		});

		// Mixins have their own identifier namespace due to
		// special definiton and reference syntax.
		check_unique_mixin_identifiers(self, report);

		let (_mixins, items): (Vec<Mixin>, Vec<Item>) = mem::take(&mut self.0)
			.into_iter()
			.partition_map(|v| match v {
				Item::Mixin(m) => itertools::Either::Left(m),
				i => itertools::Either::Right(i),
			});
		self.0 = items;

		let mut mixins = HashMap::new();
		for mixin in _mixins {
			mixins.insert_unique(mixin.id(), mixin);
		}

		for table in self.0.iter_mut().filter_map(Item::as_table_mut) {
			loop {
				let mut assimilated_mixins = HashSet::new();
				let table_mixins = std::mem::take(&mut table.mixins);
				if table_mixins.is_empty() {
					break;
				}
				for mixin in table_mixins {
					if !assimilated_mixins.insert(mixin) {
						panic!(
							"mixin {} was applied twice to table {}",
							mixin.name(),
							table.id().name()
						);
					}
					let mixin = mixins.get(&mixin).expect("unknown mixin identifier");
					table.assimilate_mixin(mixin);
				}
			}
		}

		// Currently, no passes generate new items, nor
		// alter identifiers, it should be moved to the end.
		check_unique_identifiers(self, report);

		let mut propagated_scalars = HashMap::new();
		for s in self.0.iter_mut().filter_map(Item::as_scalar_mut) {
			let inline = s.is_always_inline() || !options.generator_supports_domain;
			let data = s.propagate(inline);
			propagated_scalars.insert_unique(s.id(), data);
		}

		// Propagate scalar attributes to fields
		for composite in self.0.iter_mut().filter_map(Item::as_composite_mut) {
			for (id, data) in &propagated_scalars {
				composite.propagate_scalar_data(*id, data);
			}
			composite.process();
		}

		// Propagate composite attributes to other composites
		loop {
			let mut extended_this_step = <HashMap<TypeIdent, PropagatedScalarData>>::new();
			for s in self.0.iter_mut().filter_map(Item::as_composite_mut) {
				s.process();
				let to_propagate = s.propagate();
				let existing = extended_this_step.entry(s.id()).or_default();
				existing.extend(to_propagate);
			}

			if extended_this_step.values().all(|v| v.is_empty()) {
				break;
			}

			// Propagate newly discovered propagations
			for composite in self.0.iter_mut().filter_map(Item::as_composite_mut) {
				for (id, data) in &extended_this_step {
					composite.propagate_scalar_data(*id, data);
				}
				composite.process();
			}

			for (id, data) in extended_this_step {
				let existing = propagated_scalars.entry(id).or_default();
				existing.extend(data);
			}
		}

		// Propagate scalar attributes to columns
		for table in self.0.iter_mut().filter_map(Item::as_table_mut) {
			for (id, data) in &propagated_scalars {
				table.propagate_scalar_data(*id, data);
			}
			table.process();
		}

		match options.naming_convention {
			NamingConvention::Postgres => (Pgnc(self)).process_naming(rn),
		};
	}
	pub fn material_items(&self) -> Vec<SchemaItem<'_>> {
		self.0
			.iter()
			.filter_map(|v| {
				Some(match v {
					Item::Table(table) => SchemaItem::Table(SchemaTable {
						schema: self,
						table,
					}),
					Item::Enum(en) => SchemaItem::Enum(SchemaEnum { schema: self, en }),
					Item::Scalar(scalar) if !scalar.is_always_inline() => {
						SchemaItem::Scalar(SchemaScalar {
							schema: self,
							scalar,
						})
					}
					Item::Composite(composite) => SchemaItem::Composite(SchemaComposite {
						schema: self,
						composite,
					}),
					Item::View(view) => SchemaItem::View(SchemaView { schema: self, view }),
					_ => return None,
				})
			})
			.collect()
	}
	pub fn items(&self) -> Vec<SchemaItem<'_>> {
		self.0
			.iter()
			.map(|v| match v {
				Item::Table(table) => SchemaItem::Table(SchemaTable {
					schema: self,
					table,
				}),
				Item::Enum(en) => SchemaItem::Enum(SchemaEnum { schema: self, en }),
				Item::Scalar(scalar) => SchemaItem::Scalar(SchemaScalar {
					schema: self,
					scalar,
				}),
				Item::Composite(composite) => SchemaItem::Composite(SchemaComposite {
					schema: self,
					composite,
				}),
				Item::View(view) => SchemaItem::View(SchemaView { schema: self, view }),
				Item::Mixin(_) => unreachable!("mixins are assimilted at the earliest stage"),
			})
			.collect()
	}
	pub fn schema_ty(&self, name: TypeIdent) -> SchemaType<'_> {
		if let Some(scalar) = self.scalars().find(|s| s.id() == name) {
			return SchemaType::Scalar(SchemaScalar {
				schema: self,
				scalar,
			});
		}
		if let Some(en) = self.enums().find(|s| s.id() == name) {
			return SchemaType::Enum(SchemaEnum { schema: self, en });
		}
		if let Some(composite) = self.composites().find(|s| s.id() == name) {
			return SchemaType::Composite(SchemaComposite {
				schema: self,
				composite,
			});
		}
		panic!("schema type not found: {name:?}")
	}
	pub fn native_type(
		&self,
		name: &TypeIdent,
		rn: &RenameMap,
		report: &mut Report,
	) -> DbNativeType {
		for item in self.0.iter() {
			match item {
				Item::Enum(e) if &e.id() == name => return e.db_type(rn),
				Item::Scalar(v) if &v.id() == name => {
					return SchemaScalar {
						schema: self,
						scalar: v,
					}
					.native(rn, report);
				}
				Item::Composite(c) if &c.id() == name => return c.db_type(rn),
				_ => continue,
			}
		}
		report
			.error("native type not found")
			.annotate("referenced here", name.span());
		return DbNativeType::new("ERROR");
	}
	pub fn tables(&self) -> impl Iterator<Item = &Table> {
		self.0.iter().filter_map(Item::as_table)
	}
	pub fn views(&self) -> impl Iterator<Item = &View> {
		self.0.iter().filter_map(Item::as_view)
	}
	pub fn enums(&self) -> impl Iterator<Item = &Enum> {
		self.0.iter().filter_map(Item::as_enum)
	}
	pub fn scalars(&self) -> impl Iterator<Item = &Scalar> {
		self.0.iter().filter_map(Item::as_scalar)
	}
	pub fn composites(&self) -> impl Iterator<Item = &Composite> {
		self.0.iter().filter_map(Item::as_composite)
	}

	pub fn schema_table(&self, name: &TableIdent) -> Option<SchemaTable<'_>> {
		self.tables()
			.find(|t| &t.id() == name)
			.map(|t| SchemaTable {
				schema: self,
				table: t,
			})
	}

	pub fn schema_table_or_view(&self, name: &TableIdent) -> Option<SchemaTableOrView<'_>> {
		if let Some(v) = self
			.tables()
			.find(|t| &t.id() == name)
			.map(|t| SchemaTable {
				schema: self,
				table: t,
			})
			.map(SchemaTableOrView::Table)
		{
			return Some(v);
		}
		self.views()
			.find(|t| t.id() == Ident::unchecked_cast(*name))
			.map(|t| SchemaView {
				schema: self,
				view: t,
			})
			.map(SchemaTableOrView::View)
	}

	pub fn schema_scalar(&self, scalar: TypeIdent) -> SchemaScalar<'_> {
		SchemaScalar {
			schema: self,
			scalar: self
				.scalars()
				.find(|c| c.id() == scalar)
				.expect("scalar not found: {scalar:?}"),
		}
	}

	pub fn db_tables(&self, rn: &RenameMap) -> Vec<DbTable> {
		self.tables().map(|t| t.db(rn)).collect()
	}
	pub fn db_enums(&self, rn: &RenameMap) -> Vec<DbType> {
		self.enums().map(|t| t.db(rn)).collect()
	}
	pub fn sql<'a>(&'a self, sql: &'a Sql) -> SchemaSql<'a> {
		SchemaSql { schema: self, sql }
	}
}
