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

#[derive(Debug, Clone)]
pub struct AnnotationField {
	pub key: String,
	pub value: AnnotationValue,
}

#[derive(Debug, Clone)]
pub struct Annotation {
	pub name: String,
	pub fields: Vec<AnnotationField>,
}

pub struct DuplicateAnnotationError;
impl From<DuplicateAnnotationError> for &'static str {
	fn from(_value: DuplicateAnnotationError) -> Self {
		"duplicate annotation"
	}
}

#[derive(Debug, Clone)]
pub struct AnnotationList(pub Vec<Annotation>);
impl AnnotationList {
	pub fn iter_fields(&self, attre: &str, fielde: &str, mut cb: impl FnMut(&AnnotationValue)) {
		for attr in &self.0 {
			if attr.name != attre {
				continue;
			}
			for field in &attr.fields {
				if field.key != fielde {
					continue;
				}
				cb(&field.value);
			}
		}
	}
	pub fn get_multi<T>(&self, attre: &str, fielde: &str) -> Result<Vec<T>, T::Error>
	where
		T: TryFrom<AnnotationValue>,
	{
		let mut value = Vec::new();
		self.iter_fields(attre, fielde, |v| value.push(v.clone()));
		let mut parsed = Vec::new();
		for v in value {
			parsed.push(T::try_from(v)?);
		}
		Ok(parsed)
	}
	pub fn try_get_single<T>(&self, attre: &str, fielde: &str) -> Result<Option<T>, T::Error>
	where
		T: TryFrom<AnnotationValue>,
		T::Error: From<DuplicateAnnotationError>,
	{
		let v = self.get_multi::<T>(attre, fielde)?;
		if v.len() > 1 {
			return Err(DuplicateAnnotationError.into());
		}
		Ok(v.into_iter().next())
	}
	pub fn get_single<T>(&self, attre: &str, fielde: &str) -> Result<T, T::Error>
	where
		T: TryFrom<AnnotationValue>,
		T::Error: From<DuplicateAnnotationError>,
	{
		let v = self.get_multi::<T>(attre, fielde)?;
		if v.len() > 1 {
			return Err(DuplicateAnnotationError.into());
		}
		match v.into_iter().next() {
			Some(v) => Ok(v),
			_ => T::try_from(AnnotationValue::Unset),
		}
	}
}
