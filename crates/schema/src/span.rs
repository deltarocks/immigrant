use std::cell::RefCell;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SourceId(u32);

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct SimpleSpan {
	source_id: SourceId,
	pub start: u32,
	pub end: u32,
}
impl SimpleSpan {
	pub(crate) fn new(source_id: SourceId, start: u32, end: u32) -> Self {
		Self {
			source_id,
			start,
			end,
		}
	}
	pub fn source(&self) -> SourceId {
		self.source_id
	}
}

struct Source {
	path: String,
	text: String,
}

thread_local! {
	static SPANS: RefCell<Vec<Source>> = Default::default();
}

pub(crate) fn register_source(path: String, text: String) -> SourceId {
	SPANS.with(|src| {
		let mut src = src.borrow_mut();
		// As sources can't be removed - there is no holes
		let id = SourceId(src.len() as u32);
		src.push(Source { path, text });
		id
	})
}
impl SourceId {
	pub fn path(&self) -> String {
		SPANS.with(|srcs| {
			srcs.borrow()
				.get(self.0 as usize)
				.expect("source is not registered?")
				.path
				.clone()
		})
	}
	pub fn text(&self) -> String {
		SPANS.with(|srcs| {
			srcs.borrow()
				.get(self.0 as usize)
				.expect("source is not registered?")
				.text
				.clone()
		})
	}
}
