//! Types for spatial item extraction: what it returns, and the two row shapes
//! it works in.

use crate::common::ReceiptWarningKind;
use crate::money::Money;

#[derive(Clone, Debug)]
pub(crate) struct SpatialExtractedItem {
    pub description: String,
    pub price: Money,
}

#[derive(Clone, Debug)]
pub(crate) struct SpatialParserWarning {
    pub kind: ReceiptWarningKind,
    pub message: String,
    pub after_item_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct SpatialExtractionOutcome {
    pub items: Vec<SpatialExtractedItem>,
    pub warnings: Vec<SpatialParserWarning>,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedLine {
    pub(crate) line_y: f64,
    pub(crate) full_text: String,
    pub(crate) left_text: String,
    /// Set by [`mark_annotation_columns`]: this row stands in a grid column that
    /// carries no price anywhere on the receipt, so it annotates the item above
    /// rather than being one.
    pub(crate) is_annotation: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct PriceCandidate {
    pub(crate) price_y: f64,
    pub(crate) price_scaled: i64,
    pub(crate) source_line_index: usize,
}

pub(crate) struct AnnotationRow {
    pub(crate) left_x: f64,
    pub(crate) left_text: String,
    pub(crate) has_price: bool,
}
