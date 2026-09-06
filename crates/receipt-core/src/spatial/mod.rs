//! Spatial item extraction for receipts whose prices sit in their own column.
//!
//! The counterpart to [`text`](crate::text): where that path reads the line as a
//! string, this one reads the word boxes, so it can pair a description with the
//! price printed beside it even when OCR grouped them onto different rows.
//!
//! [`rows`] reads geometry and row roles; [`pairing`] selects a description;
//! [`engine`] emits items and owns row claims. Both extraction paths return
//! the shared [`crate::extraction::ExtractionOutcome`].

mod candidate;
mod engine;
mod pairing;
mod patterns;
mod rows;
mod types;

#[cfg(test)]
mod tests;

pub(crate) use engine::extract_spatial_items;
pub(crate) use rows::annotation_line_flags;
