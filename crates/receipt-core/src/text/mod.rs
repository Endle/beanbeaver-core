//! Text-line item extraction for grocery-style receipts.
//!
//! Split for maintainability. [`extract_text_items`] is the entry point;
//! [`types::ParsedTextItem`] and [`types::TextParserWarning`] are its return
//! types, named through `types` because nothing outside this module refers to
//! them by name — `parser` binds them positionally.

mod engine;
mod patterns;
mod types;

#[cfg(test)]
mod tests;

pub(crate) use engine::extract_text_items;
