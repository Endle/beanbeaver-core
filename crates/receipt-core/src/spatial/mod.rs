//! Spatial item extraction for receipts whose prices sit in their own column.
//!
//! The counterpart to [`text`](crate::text): where that path reads the line as a
//! string, this one reads the word boxes, so it can pair a description with the
//! price printed beside it even when OCR grouped them onto different rows.
//!
//! Split the same way `text` is — [`engine`] applies, [`patterns`] holds the
//! shapes it applies, [`types`] the row and result shapes — plus [`candidate`],
//! which owns the one decision complex enough to test on its own: which of
//! several eligible rows a price belongs to.

mod candidate;
mod engine;
mod patterns;
mod types;

#[cfg(test)]
mod tests;

// The whole surface. `parser` binds the outcome's fields positionally and never
// names its types, exactly as it does for `text` — so they stay inside `types`.
pub(crate) use engine::{annotation_line_flags, extract_spatial_items};
