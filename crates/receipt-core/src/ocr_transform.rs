//! Port of `receipt/ocr_helpers.py::transform_paddleocr_result` and the
//! `receipt/detection_normalization.py` pipeline orchestration into pure Rust.
//!
//! Input: raw PaddleOCR detections (padded-image pixel coordinates). Output: one
//! [`OcrDocument`] — the line/word grouping, normalized to `[0,1]` against the
//! de-padded image, that `parser::parse_receipt` consumes.
//!
//! This module owns the two **representation boundaries** either side of
//! [`crate::detection_normalization`]'s passes:
//!
//! ```text
//! RawDetectionPage (padded pixels, validated at construction)
//!     |  de-pad
//!     v
//! DetectionPage (de-padded pixels) --[ the configurable passes ]--> DetectionPage
//!     |  line grouping + coordinate normalization
//!     v
//! OcrDocument ([0, 1] coordinates)
//! ```
//!
//! Validation, de-padding, grouping and lowering are boundaries, not passes:
//! they always run, and nothing can turn them off.

use std::fmt;

use crate::detection_normalization::{
    normalize_detections, Detection, DetectionPage, NormalizationOptions,
};
use crate::ocr_document::{Bbox, OcrDocument, OcrLine, OcrWord};
use crate::ocr_line_grouping::group_detections_into_lines;

/// Minimum points in an OCR polygon.
///
/// PP-OCRv5 emits quads and nothing else — every one of the 12,279 detections
/// across the public and private cached corpora is 4-point, and
/// `ocr_paddle::engine::Detection` carries `[[f32; 2]; 4]`, so the count is
/// structural on the live path. The FFI seam has always enforced it (a flat
/// `points_xy` of even length >= 8) while direct Rust callers enforced nothing.
/// This is that one rule, in one place.
const MIN_POLYGON_POINTS: usize = 4;

/// One raw OCR detection: a polygon (>= [`MIN_POLYGON_POINTS`] points,
/// padded-image pixels), the recognized text, and a confidence score.
#[derive(Clone, Debug)]
pub struct RawDetection {
    pub points: Vec<(f64, f64)>,
    pub text: String,
    pub confidence: f64,
}

/// Why a [`RawDetectionPage`] could not be built.
///
/// Every variant describes input the pipeline cannot make sense of, caught at
/// the boundary rather than turning into a NaN coordinate or an underflowing
/// subtraction several passes later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransformError {
    /// Padded image dimensions must be positive and fit in a `u32`.
    PaddedDimensions { width: i64, height: i64 },
    /// The border added by `resize_and_pad` must fit twice inside the padded
    /// image, or de-padding leaves nothing (or a negative extent) behind.
    Padding {
        padding: i64,
        width: i64,
        height: i64,
    },
    /// A detection polygon has too few points to bound anything.
    Polygon { index: usize, points: usize },
    /// A coordinate or confidence was NaN or infinite. Left unchecked these
    /// poison every downstream geometry comparison *silently* — NaN compares
    /// false against everything, so a row simply stops matching.
    NonFinite { index: usize, field: &'static str },
}

impl fmt::Display for TransformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransformError::PaddedDimensions { width, height } => write!(
                f,
                "padded image dimensions must be positive and fit in u32 (got {width}x{height})"
            ),
            TransformError::Padding {
                padding,
                width,
                height,
            } => write!(
                f,
                "padding {padding} does not fit twice inside the padded image {width}x{height}"
            ),
            TransformError::Polygon { index, points } => write!(
                f,
                "detection {index}: polygon needs at least {MIN_POLYGON_POINTS} points \
                 (got {points})"
            ),
            TransformError::NonFinite { index, field } => {
                write!(f, "detection {index}: {field} is not finite")
            }
        }
    }
}

impl std::error::Error for TransformError {}

/// Raw detections plus **the image they were measured against** — validated
/// once, at construction.
///
/// Detections come back from OCR in padded-image pixels, so they are
/// meaningless without the padded dimensions and the padding to undo. Those
/// used to be three loose `i64` parameters threaded through `process_receipt`
/// and `transform`: four things a caller could get individually right and
/// collectively wrong. Constructing the page is the one place they are checked
/// against each other, and past it the coordinate-space contract is carried by
/// the type rather than by a doc comment.
///
/// Fields are private on purpose — a public field is a second constructor that
/// skips [`try_new`](Self::try_new).
#[derive(Clone, Debug)]
pub struct RawDetectionPage {
    detections: Vec<RawDetection>,
    padded_width: u32,
    padded_height: u32,
    padding: u32,
}

impl RawDetectionPage {
    /// Validate a detection list against the padded image it was measured on.
    ///
    /// Confidence is checked for finiteness only, not range: the pipeline
    /// tolerates whatever the engine reports today, and tightening that would be
    /// a behavioural change rather than a structural one.
    pub fn try_new(
        detections: Vec<RawDetection>,
        padded_width: i64,
        padded_height: i64,
        padding: i64,
    ) -> Result<Self, TransformError> {
        if padded_width <= 0
            || padded_height <= 0
            || padded_width > i64::from(u32::MAX)
            || padded_height > i64::from(u32::MAX)
        {
            return Err(TransformError::PaddedDimensions {
                width: padded_width,
                height: padded_height,
            });
        }
        // `padding <= u32::MAX` is established before doubling, and `||`
        // short-circuits, so `2 * padding` cannot overflow the i64.
        if padding < 0
            || padding > i64::from(u32::MAX)
            || 2 * padding >= padded_width
            || 2 * padding >= padded_height
        {
            return Err(TransformError::Padding {
                padding,
                width: padded_width,
                height: padded_height,
            });
        }
        for (index, det) in detections.iter().enumerate() {
            if det.points.len() < MIN_POLYGON_POINTS {
                return Err(TransformError::Polygon {
                    index,
                    points: det.points.len(),
                });
            }
            if det
                .points
                .iter()
                .any(|(x, y)| !x.is_finite() || !y.is_finite())
            {
                return Err(TransformError::NonFinite {
                    index,
                    field: "point",
                });
            }
            if !det.confidence.is_finite() {
                return Err(TransformError::NonFinite {
                    index,
                    field: "confidence",
                });
            }
        }
        Ok(Self {
            detections,
            padded_width: padded_width as u32,
            padded_height: padded_height as u32,
            padding: padding as u32,
        })
    }

    /// The detections as supplied, still in padded-image pixels.
    pub fn detections(&self) -> &[RawDetection] {
        &self.detections
    }

    pub fn padded_width(&self) -> u32 {
        self.padded_width
    }

    pub fn padded_height(&self) -> u32 {
        self.padded_height
    }

    pub fn padding(&self) -> u32 {
        self.padding
    }

    /// Width of the original image, i.e. with the OCR padding removed. Positive
    /// by construction.
    pub fn image_width(&self) -> f64 {
        f64::from(self.padded_width - 2 * self.padding)
    }

    /// Height of the original image. Positive by construction.
    pub fn image_height(&self) -> f64 {
        f64::from(self.padded_height - 2 * self.padding)
    }

    /// De-pad into the space the normalization passes work in.
    fn into_detection_page(self) -> DetectionPage {
        let image_width = self.image_width();
        let image_height = self.image_height();
        let pad = f64::from(self.padding);
        let detections = self
            .detections
            .into_iter()
            .map(|det| {
                let adjusted: Vec<(f64, f64)> =
                    det.points.iter().map(|(x, y)| (x - pad, y - pad)).collect();
                let y_coords: Vec<f64> = adjusted.iter().map(|(_, y)| *y).collect();
                let center_y = y_coords.iter().sum::<f64>() / y_coords.len() as f64;
                let y_min = y_coords.iter().cloned().fold(f64::INFINITY, f64::min);
                let y_max = y_coords.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let min_x = adjusted
                    .iter()
                    .map(|(x, _)| *x)
                    .fold(f64::INFINITY, f64::min);
                Detection {
                    confidence: det.confidence,
                    text: det.text,
                    center_y,
                    y_min,
                    y_max,
                    min_x,
                    bbox: adjusted,
                }
            })
            .collect();
        DetectionPage {
            detections,
            image_width,
            image_height,
        }
    }
}

/// Not `f64::clamp`, which clippy suggests here: the two disagree on NaN.
/// `NaN.max(0.0).min(1.0)` is `0.0` — `f64::max` returns the non-NaN operand —
/// while `NaN.clamp(0.0, 1.0)` is NaN. The callers divide a fold seeded with
/// ±INFINITY by an image dimension, so `0.0 / 0.0` on a degenerate frame reaches
/// here as NaN, and a NaN coordinate poisons every downstream geometry
/// comparison silently rather than failing. Keep the pinning behaviour.
#[allow(clippy::manual_clamp, reason = "differs from clamp on NaN; see above")]
fn clamp_unit_interval(value: f64) -> f64 {
    value.max(0.0).min(1.0)
}

/// Transform raw detections from a padded image into parser inputs, running the
/// [`NormalizationOptions::SHIPPING`] passes.
///
/// **This is the production entry point, and it takes no profile.** The scan
/// path and the UniFFI seam call only this, so there is exactly one pipeline the
/// apps can run and `device_sim` can reproduce. Infallible: everything that can
/// be rejected was rejected by [`RawDetectionPage::try_new`].
pub fn transform(page: RawDetectionPage) -> OcrDocument {
    transform_with_options(page, NormalizationOptions::SHIPPING)
}

/// [`transform`] with a chosen pass profile — **for tests and diagnostics.**
///
/// Lets a caller hold OCR output constant and vary one pass, which is how a
/// parse difference gets attributed to filtering, deskew or ordering without
/// editing production code. Deliberately not reachable through `ProcessOptions`
/// or the mobile contract: a diagnostic profile must not be able to become a
/// second shipping pipeline.
pub fn transform_with_options(
    page: RawDetectionPage,
    options: NormalizationOptions,
) -> OcrDocument {
    if page.detections.is_empty() {
        return OcrDocument::default();
    }

    let mut page = page.into_detection_page();
    normalize_detections(&mut page, options);
    lower_to_document(&page)
}

/// Group the normalized detections into lines and normalize every coordinate
/// into `[0, 1]` against the de-padded image — the second representation
/// boundary, and always run.
fn lower_to_document(page: &DetectionPage) -> OcrDocument {
    let (image_width, image_height) = (page.image_width, page.image_height);
    let detection_data = &page.detections;
    let groups = group_detections_into_lines(detection_data, image_width);

    let mut lines: Vec<OcrLine> = Vec::with_capacity(groups.len());

    for group in groups {
        let mut words = Vec::with_capacity(group.len());
        let mut texts = Vec::with_capacity(group.len());
        let mut line_height = 0.0f64;
        let mut sum_center_y = 0.0f64;
        for &idx in &group {
            let det = &detection_data[idx];
            line_height = line_height.max(det.y_max - det.y_min);
            sum_center_y += det.center_y;
            let xs: Vec<f64> = det.bbox.iter().map(|(x, _)| *x).collect();
            let ys: Vec<f64> = det.bbox.iter().map(|(_, y)| *y).collect();
            let bbox = Bbox {
                left: clamp_unit_interval(
                    xs.iter().cloned().fold(f64::INFINITY, f64::min) / image_width,
                ),
                top: clamp_unit_interval(
                    ys.iter().cloned().fold(f64::INFINITY, f64::min) / image_height,
                ),
                right: clamp_unit_interval(
                    xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max) / image_width,
                ),
                bottom: clamp_unit_interval(
                    ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max) / image_height,
                ),
            };
            texts.push(det.text.clone());
            words.push(OcrWord {
                text: det.text.clone(),
                bbox,
                confidence: det.confidence,
            });
        }
        let line_text = texts.join(" ");
        let center_y = if group.is_empty() {
            0.0
        } else {
            sum_center_y / group.len() as f64
        };
        // Both line metrics are normalized against image height, so the whole
        // document speaks one coordinate space — see `ocr_document`. They are
        // only ever read as ratios against each other, so this cannot change a
        // banner verdict; it removes the px-alongside-normalized hazard that
        // this very loop already produced a bug with once (see `normalize`).
        lines.push(OcrLine {
            text: line_text,
            words,
            height: line_height / image_height,
            center_y: center_y / image_height,
        });
    }

    OcrDocument { lines }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Axis-aligned rectangle detection in padded-image pixels (4 CW points),
    /// mirroring the desktop test helper `_bbox(x0, y0, x1, y1)`.
    fn rect(x0: f64, y0: f64, x1: f64, y1: f64, text: &str, conf: f64) -> RawDetection {
        RawDetection {
            points: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
            text: text.to_string(),
            confidence: conf,
        }
    }

    /// The validated page the transform now takes. Panics on invalid input,
    /// which is what a test wants: every fixture here is meant to be valid.
    fn page(
        detections: Vec<RawDetection>,
        padded_width: i64,
        padded_height: i64,
        padding: i64,
    ) -> RawDetectionPage {
        RawDetectionPage::try_new(detections, padded_width, padded_height, padding)
            .expect("test fixture is a valid detection page")
    }

    /// Two real item rows, each overlapped by a Costco "bottom of basket"
    /// banner. Exercises the BOB pass, and is well-formed enough for the other
    /// three to run over it.
    fn bob_marker_fixture() -> Vec<RawDetection> {
        vec![
            rect(
                20.0,
                200.0,
                820.0,
                240.0,
                "*xxxxxxxxxxBottom of Baske xxxxxxxxxxx",
                0.95,
            ),
            rect(120.0, 210.0, 500.0, 250.0, "232952 COKE ZERO", 0.99),
            rect(760.0, 210.0, 920.0, 248.0, "17.19 H", 0.99),
            rect(40.0, 300.0, 500.0, 340.0, "*x*********BOB Count 3", 0.95),
            rect(120.0, 320.0, 550.0, 360.0, "305882 *KS IBU 400M", 0.99),
            rect(760.0, 324.0, 900.0, 356.0, "16.99", 0.99),
        ]
    }

    /// Ported from desktop `tests/test_ocr_helpers.py::
    /// test_transform_filters_overlapping_bob_markers_keeps_real_item_lines`.
    /// BOB ("bottom of basket") marker lines that overlap real item rows must be
    /// dropped, while the item detections still group into their expected lines.
    #[test]
    fn filters_overlapping_bob_markers_keeps_real_item_lines() {
        let dets = bob_marker_fixture();

        // padding = 0 => padded dims == original dims (1000x1200).
        let out = transform(page(dets, 1000, 1200, 0));
        let full_text = out.full_text();

        assert!(
            !full_text.contains("Bottom of Baske"),
            "bob marker leaked: {full_text}"
        );
        assert!(
            !full_text.contains("BOB Count 3"),
            "bob marker leaked: {full_text}"
        );
        assert!(
            full_text.contains("232952 COKE ZERO 17.19 H"),
            "item row not grouped: {full_text}"
        );
        assert!(
            full_text.contains("305882 *KS IBU 400M 16.99"),
            "item row not grouped: {full_text}"
        );

        // Word bboxes are normalized into the unit interval.
        let bbox = &out.lines[0].words[0].bbox;
        for v in [bbox.left, bbox.top, bbox.right, bbox.bottom] {
            assert!((0.0..=1.0).contains(&v), "bbox coord {v} outside [0,1]");
        }
        assert!(bbox.left <= bbox.right && bbox.top <= bbox.bottom);
    }

    /// Empty input yields an empty document, not a malformed one.
    #[test]
    fn empty_detections_yield_an_empty_document() {
        let out = transform(page(Vec::new(), 1000, 1200, 50));
        assert!(out.full_text().is_empty());
        assert!(out.lines.is_empty());
        assert!(!out.has_useful_bbox_data());
    }

    /// Padding is subtracted before normalization: coordinates in padded space are
    /// de-padded, then divided by the original (de-padded) dims.
    #[test]
    fn padding_is_removed_before_normalization() {
        // padded 200x200 with padding 50 => original 100x100.
        // Rect (50,50)-(150,90) de-pads to (0,0)-(100,40) => left 0, right 1, bottom .4.
        let out = transform(page(
            vec![rect(50.0, 50.0, 150.0, 90.0, "HELLO", 0.99)],
            200,
            200,
            50,
        ));
        assert_eq!(out.full_text(), "HELLO");
        let bbox = &out.lines[0].words[0].bbox;
        assert!((bbox.left - 0.0).abs() < 1e-9, "left={}", bbox.left);
        assert!((bbox.right - 1.0).abs() < 1e-9, "right={}", bbox.right);
        assert!((bbox.top - 0.0).abs() < 1e-9, "top={}", bbox.top);
        assert!((bbox.bottom - 0.4).abs() < 1e-9, "bottom={}", bbox.bottom);
    }

    /// The line metrics live in the same normalized space as the boxes — the
    /// units invariant `ocr_document` exists to hold. Same rect as above: 40px
    /// tall, centered at y=20, on a 100px-tall de-padded image.
    #[test]
    fn line_metrics_are_normalized_against_image_height() {
        let out = transform(page(
            vec![rect(50.0, 50.0, 150.0, 90.0, "HELLO", 0.99)],
            200,
            200,
            50,
        ));
        let line = &out.lines[0];
        assert!((line.height - 0.4).abs() < 1e-9, "height={}", line.height);
        assert!(
            (line.center_y - 0.2).abs() < 1e-9,
            "center_y={}",
            line.center_y
        );
    }

    // ---- RawDetectionPage validation -------------------------------------

    #[test]
    fn rejects_non_positive_or_oversized_dimensions() {
        for (w, h) in [
            (0, 1200),
            (1000, 0),
            (-1, 1200),
            (i64::from(u32::MAX) + 1, 1200),
        ] {
            let err = RawDetectionPage::try_new(Vec::new(), w, h, 0)
                .expect_err("dimensions {w}x{h} must be rejected");
            assert_eq!(
                err,
                TransformError::PaddedDimensions {
                    width: w,
                    height: h
                }
            );
        }
    }

    /// De-padding subtracts the border twice, so a padding that does not fit
    /// twice would leave a zero or negative image extent to divide by.
    #[test]
    fn rejects_padding_that_does_not_fit_twice() {
        for pad in [-1, 500, 600, i64::from(u32::MAX) + 1] {
            let err = RawDetectionPage::try_new(Vec::new(), 1000, 1200, pad)
                .expect_err("padding {pad} must be rejected");
            assert!(matches!(err, TransformError::Padding { .. }), "{err}");
        }
        // The exact boundary: 2*499 < 1000 is fine, 2*500 is not.
        assert!(RawDetectionPage::try_new(Vec::new(), 1000, 1200, 499).is_ok());
    }

    /// The rule the FFI seam has always enforced and direct Rust callers never
    /// did. Both now go through this one check.
    #[test]
    fn rejects_polygons_with_too_few_points() {
        let stub = RawDetection {
            points: vec![(0.0, 0.0), (10.0, 10.0)],
            text: "HI".to_string(),
            confidence: 0.9,
        };
        let err = RawDetectionPage::try_new(vec![stub], 1000, 1200, 0)
            .expect_err("a 2-point polygon must be rejected");
        assert_eq!(
            err,
            TransformError::Polygon {
                index: 0,
                points: 2
            }
        );
    }

    /// NaN never *fails* downstream — it compares false against everything and
    /// quietly stops a row from matching. Catch it at the boundary instead.
    #[test]
    fn rejects_non_finite_coordinates_and_confidence() {
        let mut bad_point = rect(0.0, 0.0, 10.0, 10.0, "HI", 0.9);
        bad_point.points[2].1 = f64::NAN;
        assert_eq!(
            RawDetectionPage::try_new(vec![bad_point], 1000, 1200, 0).expect_err("NaN point"),
            TransformError::NonFinite {
                index: 0,
                field: "point"
            }
        );

        let mut bad_conf = rect(0.0, 0.0, 10.0, 10.0, "HI", 0.9);
        bad_conf.confidence = f64::INFINITY;
        assert_eq!(
            RawDetectionPage::try_new(vec![bad_conf], 1000, 1200, 0).expect_err("inf confidence"),
            TransformError::NonFinite {
                index: 0,
                field: "confidence"
            }
        );
    }

    /// Confidence is range-*tolerant* on purpose: whatever the engine reports
    /// reaches the low-quality pass, which is the thing that judges it.
    #[test]
    fn tolerates_out_of_range_but_finite_confidence() {
        let det = rect(0.0, 0.0, 10.0, 10.0, "HI", 1.5);
        assert!(RawDetectionPage::try_new(vec![det], 1000, 1200, 0).is_ok());
    }

    /// The page carries the de-padded dimensions the passes and the lowering
    /// both divide by, so the two can no longer be handed different images.
    #[test]
    fn page_reports_de_padded_dimensions() {
        let p = page(Vec::new(), 200, 300, 50);
        assert_eq!(p.padded_width(), 200);
        assert_eq!(p.padding(), 50);
        assert!((p.image_width() - 100.0).abs() < 1e-9);
        assert!((p.image_height() - 200.0).abs() < 1e-9);
    }

    // ---- pass profiles ----------------------------------------------------

    /// `transform` is `SHIPPING`, exactly. If this ever diverges, the apps are
    /// running a profile `transform_with_options` cannot reproduce.
    #[test]
    fn transform_is_the_shipping_profile() {
        let dets = bob_marker_fixture();
        let shipped = transform(page(dets.clone(), 1000, 1200, 0));
        let explicit =
            transform_with_options(page(dets, 1000, 1200, 0), NormalizationOptions::SHIPPING);
        assert_eq!(shipped.full_text(), explicit.full_text());
        assert_eq!(shipped.lines.len(), explicit.lines.len());
    }

    /// Ablation actually ablates: with the BOB pass off, the marker lines the
    /// shipping profile drops survive into the document. This is the whole
    /// point of the profile — a pass can be held out while OCR is held constant.
    #[test]
    fn disabling_a_pass_changes_only_that_pass() {
        let dets = bob_marker_fixture();
        let without_bob = transform_with_options(
            page(dets, 1000, 1200, 0),
            NormalizationOptions {
                filter_bob_markers: false,
                ..NormalizationOptions::SHIPPING
            },
        );
        let text = without_bob.full_text();
        assert!(
            text.contains("Bottom of Baske"),
            "BOB pass was disabled but its markers are still gone: {text}"
        );
        // The other passes still ran: reading order put the top row first.
        assert!(
            without_bob.lines[0].center_y <= without_bob.lines[1].center_y,
            "reading-order pass did not run"
        );
    }

    /// Every pass off is still a valid document — the boundaries (de-padding,
    /// grouping, `[0,1]` normalization) are not passes and always run.
    #[test]
    fn all_passes_disabled_still_lowers_a_document() {
        let out = transform_with_options(
            page(bob_marker_fixture(), 1000, 1200, 0),
            NormalizationOptions {
                filter_low_quality: false,
                filter_bob_markers: false,
                deskew: false,
                sort_reading_order: false,
            },
        );
        assert!(!out.lines.is_empty());
        for line in &out.lines {
            for word in &line.words {
                for v in [
                    word.bbox.left,
                    word.bbox.top,
                    word.bbox.right,
                    word.bbox.bottom,
                ] {
                    assert!((0.0..=1.0).contains(&v), "bbox coord {v} outside [0,1]");
                }
            }
        }
    }
}
