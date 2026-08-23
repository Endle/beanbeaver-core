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

/// What stage 4 decided about one price: which row it belongs to, and the three
/// facts about *how* it decided that the emission stage still needs.
#[derive(Clone, Debug)]
pub(crate) struct LineChoice {
    pub(crate) line_index: Option<usize>,
    pub(crate) distance: f64,
    /// Stage 3's value, possibly raised by stage 4.
    pub(crate) prefer_below: bool,
    /// The nearest-row search found the *next priced row* for a source row that
    /// carries no description of its own. That is not a pairing, and it must not
    /// fall through to the search-above fallback either.
    pub(crate) suppress_fallback: bool,
    /// The source row is a bare code repeating an item already priced above it,
    /// so a miss here is expected and must not be warned about.
    pub(crate) source_repeats_previous_priced_item: bool,
    /// The price was moved onto an unpriced deposit stub below its own row.
    pub(crate) shifted_to_deposit_stub: bool,
}
