use std::cell::RefCell;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct SourceId(u32);

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
#[allow(dead_code)]
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
}

thread_local! {
	static SPANS: RefCell<Vec<String>> = Default::default();
}

pub(crate) fn register_source(text: String) -> SourceId {
	SPANS.with(|src| {
		let mut src = src.borrow_mut();
		// As sources can't be removed - there is no holes
		let id = SourceId(src.len() as u32);
		src.push(text);
		id
	})
}
#[allow(dead_code)]
pub(crate) fn get_source(src: SourceId) -> String {
	SPANS.with(|srcs| {
		let srcs = srcs.borrow();
		srcs.get(src.0 as usize)
			.expect("source is not registered?")
			.clone()
	})
}
