//! Text-line item extraction for grocery-style receipts.
//!
//! Split for maintainability; public surface is unchanged:
//! [`ParsedTextItem`], [`TextParserWarning`], [`extract_text_items`].

mod engine;
mod patterns;
mod types;

#[cfg(test)]
mod tests;

pub use engine::extract_text_items;
pub use types::{ParsedTextItem, TextParserWarning};
