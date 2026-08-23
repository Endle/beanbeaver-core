//! The OCR document: one receipt's recognized text and geometry, in one shape.
//!
//! This type replaces the three parallel views `ocr_transform` used to build in
//! a single loop — `full_text`, the merchant helper pages, and the spatial pages
//! — which the parser then took as three positional parameters and had to
//! re-check for alignment at runtime. One document makes that invariant
//! structural: the lines are the same lines, so an index into one is an index
//! into all of them.
//!
//! # Coordinate space
//!
//! Everything here is **normalized to `[0, 1]` against the de-padded,
//! original-image dimensions** — `Bbox` against width and height, `OcrLine`'s
//! `height` and `center_y` against height alone. That is the whole point of the
//! type: the previous shape carried normalized boxes and *pixel* line metrics on
//! the same line, in bare `f64`s, which is the units hazard
//! `docs/architecture.md` warns about and the one that already produced a real
//! bug (see `ocr_transform::normalize`).
//!
//! The line metrics are deliberately **not** clamped to `[0, 1]` the way `Bbox`
//! is. They are only ever read as ratios against each other — a median height, a
//! fraction of the page's own `center_y` span — so clamping would compress those
//! spans without making any comparison more meaningful.

/// A normalized `[0, 1]` bounding box in de-padded original-image space.
#[derive(Clone, Debug, PartialEq)]
pub struct Bbox {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// One recognized OCR detection: its text, where it sits, and how sure the
/// recognizer was.
#[derive(Clone, Debug)]
pub struct OcrWord {
    pub text: String,
    pub bbox: Bbox,
    pub confidence: f64,
}

/// One grouped line: the words `ocr_line_grouping` put on the same row, their
/// texts joined with single spaces, plus the two line-level metrics the merchant
/// banner search needs.
#[derive(Clone, Debug)]
pub struct OcrLine {
    pub text: String,
    pub words: Vec<OcrWord>,
    /// Tallest word-box height on this line, normalized against image height. A
    /// large-font store banner sits well above body-text height, which is what
    /// drives the size-prior in `parse_helpers`.
    pub height: f64,
    /// Line center Y, normalized against image height. Restricts the banner
    /// search to the receipt top.
    pub center_y: f64,
}

impl OcrLine {
    /// A line with no line-level geometry — `height` and `center_y` are left at
    /// zero, meaning "unmeasured".
    ///
    /// For callers that have word boxes but never measured the line: the spatial
    /// item path reads only `Bbox`, so its tests build lines this way. The
    /// banner search skips zero-height lines rather than ranking them.
    pub fn new(text: impl Into<String>, words: Vec<OcrWord>) -> Self {
        Self {
            text: text.into(),
            words,
            height: 0.0,
            center_y: 0.0,
        }
    }
}

/// One receipt, as recognized: its lines in reading order.
///
/// There is no page dimension. The OCR path has always produced exactly one page
/// — every construction site built `vec![page]` and every reader took `pages[0]`
/// — a leftover from the PaddleOCR port that cost an indirection everywhere and
/// bought nothing.
#[derive(Clone, Debug, Default)]
pub struct OcrDocument {
    pub lines: Vec<OcrLine>,
}

impl OcrDocument {
    /// The document's text: every line joined with `\n`.
    ///
    /// No line's own text contains a newline (it is a join of word texts with
    /// spaces), so `full_text().lines()` is 1:1 with `self.lines` — the property
    /// the parser used to have to check at runtime.
    pub fn full_text(&self) -> String {
        let mut out = String::new();
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            out.push_str(&line.text);
        }
        out
    }

    /// A document with text but no geometry, one line per line of `text`.
    ///
    /// For callers that have recognized text and nothing else — the parser's own
    /// text-path tests, and any consumer that never ran detection. Such a
    /// document reports [`has_useful_bbox_data`](Self::has_useful_bbox_data)
    /// false, which is what keeps the spatial paths off it.
    ///
    /// Round-tripping through [`full_text`](Self::full_text) is exact apart from
    /// line-ending normalization: `\r\n` becomes `\n` and a trailing newline is
    /// dropped, both because `str::lines` says so.
    pub fn from_text(text: &str) -> Self {
        Self {
            lines: text
                .lines()
                .map(|line| OcrLine::new(line, Vec::new()))
                .collect(),
        }
    }

    /// Whether the document carries usable geometry, judged on the header region
    /// the merchant search actually reads.
    ///
    /// A text-only caller (the parser's own text-path tests, or a consumer that
    /// passes no detections at all) has no words, so the spatial paths must not
    /// be trusted with it.
    pub fn has_useful_bbox_data(&self) -> bool {
        self.lines
            .iter()
            .take(10)
            .any(|line| !line.words.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            bbox: Bbox {
                left: 0.0,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            },
            confidence: 0.99,
        }
    }

    #[test]
    fn full_text_is_one_to_one_with_lines() {
        let doc = OcrDocument {
            lines: vec![
                OcrLine::new("COSTCO WHOLESALE", vec![word("COSTCO"), word("WHOLESALE")]),
                OcrLine::new("BANANAS 1.99", vec![word("BANANAS"), word("1.99")]),
            ],
        };
        let full_text = doc.full_text();
        assert_eq!(full_text, "COSTCO WHOLESALE\nBANANAS 1.99");
        assert_eq!(full_text.lines().count(), doc.lines.len());
    }

    #[test]
    fn empty_document_has_empty_text_and_no_geometry() {
        let doc = OcrDocument::default();
        assert_eq!(doc.full_text(), "");
        assert!(!doc.has_useful_bbox_data());
    }

    #[test]
    fn from_text_round_trips_through_full_text() {
        let doc = OcrDocument::from_text("COSTCO WHOLESALE\nBANANAS 1.99");
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.full_text(), "COSTCO WHOLESALE\nBANANAS 1.99");
        assert!(!doc.has_useful_bbox_data());
    }

    #[test]
    fn a_line_without_words_carries_no_geometry() {
        // The text path builds lines this way; `has_useful_bbox_data` is what
        // keeps the spatial extractor off them.
        let doc = OcrDocument {
            lines: vec![OcrLine::new("BANANAS 1.99", Vec::new())],
        };
        assert!(!doc.has_useful_bbox_data());
    }
}
