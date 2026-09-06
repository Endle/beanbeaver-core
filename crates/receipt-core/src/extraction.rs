//! Shared output of the text and spatial item extractors.
use crate::common::ReceiptWarning;
use crate::money::Money;

#[derive(Clone, Debug)]
pub(crate) struct ExtractedItem {
    pub description: String,
    /// Printed text selected for classification, before display expansion.
    pub category_source: String,
    pub price: Money,
    pub quantity: i32,
}

#[derive(Clone, Debug)]
pub(crate) struct ExtractionOutcome {
    pub items: Vec<ExtractedItem>,
    pub warnings: Vec<ReceiptWarning>,
}
