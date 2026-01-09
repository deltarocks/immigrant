#[derive(Clone, Debug)]
pub enum AttributeValue {
	// Field is not set
	Unset,
	// Field is set, but value is not specified
	Set,
	String(String),
}
impl TryFrom<AttributeValue> for bool {
	type Error = &'static str;

	fn try_from(value: AttributeValue) -> Result<Self, Self::Error> {
		Ok(match value {
			AttributeValue::Unset => false,
			AttributeValue::Set => true,
			AttributeValue::String(_) => return Err("expected boolean, got string"),
		})
	}
}
impl TryFrom<AttributeValue> for String {
	type Error = &'static str;

	fn try_from(value: AttributeValue) -> Result<Self, Self::Error> {
		Ok(match value {
			AttributeValue::Unset => return Err("missing string attribute"),
			AttributeValue::Set => return Err("missing attribute value"),
			AttributeValue::String(s) => s,
		})
	}
}

#[derive(Debug, Clone)]
pub struct AttributeField {
	pub key: String,
	pub value: AttributeValue,
}

#[derive(Debug, Clone)]
pub struct Attribute {
	pub name: String,
	pub fields: Vec<AttributeField>,
}

pub struct DuplicateAttributeError;
impl From<DuplicateAttributeError> for &'static str {
	fn from(_value: DuplicateAttributeError) -> Self {
		"duplicate attribute"
	}
}

#[derive(Debug, Clone)]
pub struct AttributeList(pub Vec<Attribute>);
impl AttributeList {
	pub fn iter_fields(&self, attre: &str, fielde: &str, mut cb: impl FnMut(&AttributeValue)) {
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
		T: TryFrom<AttributeValue>,
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
		T: TryFrom<AttributeValue>,
		T::Error: From<DuplicateAttributeError>,
	{
		let v = self.get_multi::<T>(attre, fielde)?;
		if v.len() > 1 {
			return Err(DuplicateAttributeError.into());
		}
		Ok(v.into_iter().next())
	}
	pub fn get_single<T>(&self, attre: &str, fielde: &str) -> Result<T, T::Error>
	where
		T: TryFrom<AttributeValue>,
		T::Error: From<DuplicateAttributeError>,
	{
		let v = self.get_multi::<T>(attre, fielde)?;
		if v.len() > 1 {
			return Err(DuplicateAttributeError.into());
		}
		match v.into_iter().next() { Some(v) => {
			Ok(v)
		} _ => {
			T::try_from(AttributeValue::Unset)
		}}
	}
}
