//! Port of `receipt/ocr_helpers.py::transform_paddleocr_result` and the
//! `receipt/detection_normalization.py` pipeline orchestration into pure Rust.
//!
//! Input: raw PaddleOCR detections (padded-image pixel coordinates). Output: the
//! line/word grouping (with normalized [0,1] bboxes) and the joined `full_text`
//! that `receipt_parser::parse_receipt` consumes — split into the helper-page and
//! spatial-page shapes the parser expects.

use crate::detection_normalization::{
    deskew, filter_bob_markers, filter_low_quality, sort_reading_order, Detection,
};
use crate::ocr_line_grouping::group_detections_into_lines;
use crate::receipt_parse_helpers::{MerchantLineInput, MerchantPageInput, MerchantWordInput};
use crate::receipt_spatial::{BboxInput, LineInput, PageInput, WordInput};

/// One raw OCR detection: a polygon (>=2 points, padded-image pixels), the
/// recognized text, and a confidence score.
#[derive(Clone, Debug)]
pub struct RawDetection {
    pub points: Vec<(f64, f64)>,
    pub text: String,
    pub confidence: f64,
}

/// Transformed OCR document, in the two forms the parser needs plus `full_text`.
#[derive(Clone, Debug, Default)]
pub struct TransformedOcr {
    pub full_text: String,
    pub helper_pages: Vec<MerchantPageInput>,
    pub spatial_pages: Vec<PageInput>,
}

fn clamp_unit_interval(value: f64) -> f64 {
    value.max(0.0).min(1.0)
}

/// Apply the default post-OCR pipeline: filter_low_quality -> filter_bob_markers
/// -> deskew -> sort_reading_order. Mirrors `normalize_detections` with
/// `default_detection_pipeline()` (debug-dump I/O omitted — irrelevant on device).
fn normalize(mut dets: Vec<Detection>, image_width: f64) -> Vec<Detection> {
    let keep = filter_low_quality(&dets);
    dets = keep.into_iter().map(|i| dets[i].clone()).collect();

    let keep = filter_bob_markers(&dets);
    dets = keep.into_iter().map(|i| dets[i].clone()).collect();

    let outcome = deskew(&dets, image_width);
    if let Some(new_y) = outcome.new_y {
        for (det, (center_y, y_min, y_max)) in dets.iter_mut().zip(new_y) {
            // The corner points have to move with the summary fields. Line
            // grouping reads y_min/y_max/center_y, but the spatial path builds
            // its BboxInput top/bottom straight off `bbox`, so leaving the
            // corners behind hands the two paths different geometry for the
            // same detection. Re-derive the shift from center_y rather than
            // recomputing the shear, so the two can never drift apart.
            let delta = det.center_y - center_y;
            for point in &mut det.bbox {
                point.1 -= delta;
            }
            det.center_y = center_y;
            det.y_min = y_min;
            det.y_max = y_max;
        }
    }

    let order = sort_reading_order(&dets);
    order.into_iter().map(|i| dets[i].clone()).collect()
}

/// Transform raw detections from a padded image into parser inputs.
///
/// `padded_width`/`padded_height` are the OCR-reported (padded) image dims;
/// `padding` is the white border added during pre-OCR resize. Coordinates are
/// de-padded back to original-image space before normalization, exactly as the
/// Python transform does.
pub fn transform(
    detections: Vec<RawDetection>,
    padded_width: i64,
    padded_height: i64,
    padding: i64,
) -> TransformedOcr {
    let image_width = (padded_width - 2 * padding) as f64;
    let image_height = (padded_height - 2 * padding) as f64;

    if detections.is_empty() {
        return TransformedOcr {
            full_text: String::new(),
            helper_pages: vec![MerchantPageInput { lines: Vec::new() }],
            spatial_pages: vec![PageInput { lines: Vec::new() }],
        };
    }

    let pad = padding as f64;
    let mut detection_data: Vec<Detection> = Vec::with_capacity(detections.len());
    for det in detections {
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
        detection_data.push(Detection {
            confidence: det.confidence,
            text: det.text,
            center_y,
            y_min,
            y_max,
            min_x,
            bbox: adjusted,
        });
    }

    let detection_data = normalize(detection_data, image_width);
    let groups = group_detections_into_lines(&detection_data, image_width);

    let mut full_text_lines: Vec<String> = Vec::with_capacity(groups.len());
    let mut helper_lines: Vec<MerchantLineInput> = Vec::with_capacity(groups.len());
    let mut spatial_lines: Vec<LineInput> = Vec::with_capacity(groups.len());

    for group in groups {
        let mut helper_words = Vec::with_capacity(group.len());
        let mut spatial_words = Vec::with_capacity(group.len());
        let mut texts = Vec::with_capacity(group.len());
        let mut line_height = 0.0f64;
        let mut sum_center_y = 0.0f64;
        for &idx in &group {
            let det = &detection_data[idx];
            line_height = line_height.max(det.y_max - det.y_min);
            sum_center_y += det.center_y;
            let xs: Vec<f64> = det.bbox.iter().map(|(x, _)| *x).collect();
            let ys: Vec<f64> = det.bbox.iter().map(|(_, y)| *y).collect();
            let bbox = BboxInput {
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
            helper_words.push(MerchantWordInput {
                confidence: det.confidence,
                has_bbox: true,
            });
            spatial_words.push(WordInput {
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
        full_text_lines.push(line_text.clone());
        helper_lines.push(MerchantLineInput {
            text: line_text.clone(),
            words: helper_words,
            height: line_height,
            center_y,
        });
        spatial_lines.push(LineInput {
            text: line_text,
            words: spatial_words,
        });
    }

    TransformedOcr {
        full_text: full_text_lines.join("\n"),
        helper_pages: vec![MerchantPageInput {
            lines: helper_lines,
        }],
        spatial_pages: vec![PageInput {
            lines: spatial_lines,
        }],
    }
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

    /// Ported from desktop `tests/test_ocr_helpers.py::
    /// test_transform_filters_overlapping_bob_markers_keeps_real_item_lines`.
    /// BOB ("bottom of basket") marker lines that overlap real item rows must be
    /// dropped, while the item detections still group into their expected lines.
    #[test]
    fn filters_overlapping_bob_markers_keeps_real_item_lines() {
        let dets = vec![
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
        ];

        // padding = 0 => padded dims == original dims (1000x1200).
        let out = transform(dets, 1000, 1200, 0);
        let full_text = &out.full_text;

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

        // One page each; spatial word bboxes are normalized into the unit interval.
        assert_eq!(out.helper_pages.len(), 1);
        assert_eq!(out.spatial_pages.len(), 1);
        let bbox = &out.spatial_pages[0].lines[0].words[0].bbox;
        for v in [bbox.left, bbox.top, bbox.right, bbox.bottom] {
            assert!((0.0..=1.0).contains(&v), "bbox coord {v} outside [0,1]");
        }
        assert!(bbox.left <= bbox.right && bbox.top <= bbox.bottom);
    }

    /// Empty input yields empty text and one empty page of each shape (so the
    /// parser always sees a well-formed, single-page document).
    #[test]
    fn empty_detections_yield_one_empty_page_each() {
        let out = transform(Vec::new(), 1000, 1200, 50);
        assert!(out.full_text.is_empty());
        assert_eq!(out.helper_pages.len(), 1);
        assert_eq!(out.spatial_pages.len(), 1);
        assert!(out.helper_pages[0].lines.is_empty());
        assert!(out.spatial_pages[0].lines.is_empty());
    }

    /// Padding is subtracted before normalization: coordinates in padded space are
    /// de-padded, then divided by the original (de-padded) dims.
    #[test]
    fn padding_is_removed_before_normalization() {
        // padded 200x200 with padding 50 => original 100x100.
        // Rect (50,50)-(150,90) de-pads to (0,0)-(100,40) => left 0, right 1, bottom .4.
        let out = transform(
            vec![rect(50.0, 50.0, 150.0, 90.0, "HELLO", 0.99)],
            200,
            200,
            50,
        );
        assert_eq!(out.full_text, "HELLO");
        let bbox = &out.spatial_pages[0].lines[0].words[0].bbox;
        assert!((bbox.left - 0.0).abs() < 1e-9, "left={}", bbox.left);
        assert!((bbox.right - 1.0).abs() < 1e-9, "right={}", bbox.right);
        assert!((bbox.top - 0.0).abs() < 1e-9, "top={}", bbox.top);
        assert!((bbox.bottom - 0.4).abs() < 1e-9, "bottom={}", bbox.bottom);
    }
}
