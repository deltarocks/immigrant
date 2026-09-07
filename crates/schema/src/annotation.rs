use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use crate::diagnostics::Report;
use crate::span::SimpleSpan;
use crate::uid::Uid;
use crate::{HasUid as _, SchemaComposite, SchemaEnum, SchemaTable, SchemaType, TableColumn};

#[derive(Clone, Debug)]
pub enum AnnotationValue {
	// Field is not set
	Unset,
	// Field is set, but value is not specified
	Set,
	String(String),
}
impl TryFrom<AnnotationValue> for bool {
	type Error = &'static str;

	fn try_from(value: AnnotationValue) -> Result<Self, Self::Error> {
		Ok(match value {
			AnnotationValue::Unset => false,
			AnnotationValue::Set => true,
			AnnotationValue::String(_) => return Err("expected boolean, got string"),
		})
	}
}
impl TryFrom<AnnotationValue> for String {
	type Error = &'static str;

	fn try_from(value: AnnotationValue) -> Result<Self, Self::Error> {
		Ok(match value {
			AnnotationValue::Unset => return Err("missing string annotation"),
			AnnotationValue::Set => return Err("missing annotation value"),
			AnnotationValue::String(s) => s,
		})
	}
}
impl TryFrom<AnnotationValue> for Option<String> {
	type Error = &'static str;

	fn try_from(value: AnnotationValue) -> Result<Self, Self::Error> {
		Ok(match value {
			AnnotationValue::Unset => None,
			value => Some(String::try_from(value)?),
		})
	}
}

#[derive(Debug, Clone)]
pub struct AnnotationField {
	pub key: String,
	pub value: AnnotationValue,
	pub span: SimpleSpan,
}

#[derive(Debug, Clone)]
pub struct Annotation {
	pub name: String,
	pub fields: Vec<AnnotationField>,
	pub span: SimpleSpan,
}

pub struct DuplicateAnnotationError;
impl From<DuplicateAnnotationError> for &'static str {
	fn from(_value: DuplicateAnnotationError) -> Self {
		"duplicate annotation"
	}
}

#[derive(Default)]
pub struct AnnotationTracker {
	handled: BTreeSet<String>,
	encountered: HashSet<(String, SimpleSpan)>,
}
pub struct UsedFields {
	used: BTreeMap<String, AnnotationTracker>,
	errors: Vec<(String, SimpleSpan)>,
}
impl UsedFields {
	pub fn untracked() -> Self {
		Self {
			used: BTreeMap::new(),
			errors: Vec::new(),
		}
	}
	pub fn tracked(names: impl IntoIterator<Item = &'static str>) -> Self {
		Self {
			used: names
				.into_iter()
				.map(|v| (v.to_owned(), AnnotationTracker::default()))
				.collect(),
			errors: Vec::new(),
		}
	}

	pub fn used(&mut self, attre: &str, fielde: &str) {
		if let Some(get) = self.used.get_mut(attre) {
			get.handled.insert(fielde.to_owned());
		}
		// self.used.insert((attre.to_owned(), fielde.to_owned()));
	}
	pub fn encountered(&mut self, attre: &str, fielde: &str, span: SimpleSpan) {
		if let Some(get) = self.used.get_mut(attre) {
			get.encountered.insert((fielde.to_owned(), span));
		}
		// self.used.insert((attre.to_owned(), fielde.to_owned()));
	}
	pub fn errored(&mut self, msg: &str, span: SimpleSpan) {
		self.errors.push((msg.to_owned(), span));
	}
	pub fn report_unused(&self, report: &mut Report) {
		for (attr, fields) in &self.used {
			for (name, span) in &fields.encountered {
				if fields.handled.contains(name.as_str()) {
					continue;
				}
				report
					.error(format!("unsupported attribute field: {attr}.{name}"))
					.annotate("encountered here", *span);
			}
		}
		for (err, span) in &self.errors {
			report
				.error(format!("attribute error: {err}"))
				.annotate("encountered here", *span);
		}
		// for (attr, field) in attrs {}
	}

	pub fn encountered_all(&mut self, list: &AnnotationList) {
		for attr in &list.annotations {
			for ele in &attr.fields {
				self.encountered(&attr.name, &ele.key, ele.span);
			}
		}
	}
}
pub struct Tracker {
	fields: HashMap<Uid, UsedFields>,
	names: Vec<&'static str>,
}
impl Tracker {
	pub fn new(names: impl IntoIterator<Item = &'static str>) -> Self {
		Self {
			fields: HashMap::new(),
			names: names.into_iter().collect(),
		}
	}
	pub fn fields_internal(&mut self, uid: Uid, anns: &AnnotationList) -> &mut UsedFields {
		let f = self.fields.entry(uid).or_insert_with(|| {
			if self.names.is_empty() {
				UsedFields::untracked()
			} else {
				UsedFields::tracked(self.names.clone())
			}
		});
		f.encountered_all(anns);
		f
	}
	pub fn enum_(&mut self, e: SchemaEnum<'_>) -> &mut UsedFields {
		self.fields_internal(e.uid(), &e.annotations)
	}
	pub fn composite_(&mut self, c: SchemaComposite<'_>) -> &mut UsedFields {
		self.fields_internal(c.uid(), &c.annotations)
	}
	pub fn table_(&mut self, e: SchemaTable<'_>) -> &mut UsedFields {
		self.fields_internal(e.uid(), &e.annotations)
	}
	pub fn ty(&mut self, ty: SchemaType<'_>) -> &mut UsedFields {
		match ty {
			SchemaType::Enum(e) => self.enum_(e),
			SchemaType::Scalar(s) => self.fields_internal(s.uid(), &s.annotations),
			SchemaType::Composite(c) => self.composite_(c),
		}
	}
	pub fn column(&mut self, ty: TableColumn<'_>) -> &mut UsedFields {
		self.fields_internal(ty.uid(), &ty.annotations)
	}

	pub fn report_unused(&self, report: &mut Report) {
		for (_, ele) in self.fields.iter() {
			ele.report_unused(report);
		}
	}
}

type MultiAnnotation<T> =
	Result<Vec<(T, SimpleSpan)>, (<T as TryFrom<AnnotationValue>>::Error, SimpleSpan)>;

#[derive(Debug, Clone)]
pub struct AnnotationList {
	pub annotations: Vec<Annotation>,
	pub span: SimpleSpan,
}
impl AnnotationList {
	pub fn iter_fields(
		&self,
		attre: &str,
		fielde: &str,
		mut cb: impl FnMut(&AnnotationValue, SimpleSpan),
		track: &mut UsedFields,
	) {
		for attr in &self.annotations {
			if attr.name != attre {
				continue;
			}
			for field in &attr.fields {
				track.encountered(&attr.name, &field.key, field.span);
				if field.key != fielde {
					continue;
				}
				track.used(&attr.name, &field.key);
				cb(&field.value, field.span);
			}
		}
	}
	pub fn get_multi<T>(
		&self,
		attre: &str,
		fielde: &str,
		track: &mut UsedFields,
	) -> MultiAnnotation<T>
	where
		T: TryFrom<AnnotationValue>,
	{
		let mut value = Vec::new();
		self.iter_fields(
			attre,
			fielde,
			|v, span| value.push((v.clone(), span)),
			track,
		);
		let mut parsed = Vec::new();
		for (v, span) in value {
			parsed.push((T::try_from(v).map_err(|e| (e, span))?, span));
		}
		Ok(parsed)
	}
	pub fn try_get_single<T>(
		&self,
		attre: &str,
		fielde: &str,
		track: &mut UsedFields,
	) -> Result<T, T::Error>
	where
		T: TryFrom<AnnotationValue>,
		T::Error: From<DuplicateAnnotationError>,
	{
		let v = self
			.get_multi::<T>(attre, fielde, track)
			.map_err(|(e, _)| e)?;
		if v.len() > 1 {
			return Err(DuplicateAnnotationError.into());
		}
		match v.into_iter().next() {
			Some((v, _)) => Ok(v),
			_ => T::try_from(AnnotationValue::Unset),
		}
	}
	fn get_single_or_inner<T>(
		&self,
		attre: &str,
		fielde: &str,
		track: &mut UsedFields,
		default: T,
		error_on_missing: bool,
	) -> T
	where
		T: TryFrom<AnnotationValue>,
		T::Error: From<DuplicateAnnotationError> + fmt::Display,
	{
		let v = match self.get_multi::<T>(attre, fielde, track) {
			Ok(e) => e,
			Err((e, span)) => {
				track.errored(&format!("invalid annotation {attre}.{fielde}: {e}"), span);
				return default;
			}
		};
		if v.len() > 1 {
			let span = v[1].1;
			track.errored(
				&format!("annotation should not repeat {attre}.{fielde}"),
				span,
			);
			return default;
		}
		match v.into_iter().next() {
			Some((v, _)) => v,
			_ => {
				if error_on_missing {
					track.errored(
						&format!("annotation is required {attre}.{fielde}"),
						self.span,
					);
				}
				default
			}
		}
	}
	pub fn get_single_or_error<T>(&self, attre: &str, fielde: &str, track: &mut UsedFields) -> T
	where
		T: TryFrom<AnnotationValue>,
		T::Error: From<DuplicateAnnotationError> + fmt::Display,
		T: Default,
	{
		self.get_single_or_inner::<T>(attre, fielde, track, T::default(), true)
	}
	pub fn get_single_or_default<T>(&self, attre: &str, fielde: &str, track: &mut UsedFields) -> T
	where
		T: TryFrom<AnnotationValue>,
		T::Error: From<DuplicateAnnotationError> + fmt::Display,
		T: Default,
	{
		self.get_single_or::<T>(attre, fielde, T::default(), track)
	}
	pub fn get_single_or<T>(
		&self,
		attre: &str,
		fielde: &str,
		default: T,
		track: &mut UsedFields,
	) -> T
	where
		T: TryFrom<AnnotationValue>,
		T::Error: From<DuplicateAnnotationError> + fmt::Display,
		T: Default,
	{
		self.get_single_or_inner::<T>(attre, fielde, track, default, false)
	}
	pub fn get_flag(&self, attre: &str, fielde: &str, track: &mut UsedFields) -> bool {
		self.get_single_or::<bool>(attre, fielde, false, track)
	}
}
