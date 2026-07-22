//! The fat-Rust seam's single entry: a decoded image -> structured receipt +
//! beancount, fully on-device. Mirrors the desktop flow
//! (`resize_image_bytes` -> OCR service -> `process_receipt`).

use std::time::Instant;

use image::RgbImage;
use receipt_core::ocr_transform::RawDetection;
use receipt_core::process::{process_receipt, ProcessedReceipt};

use crate::engine::OcrEngine;

/// End-to-end per-stage timings (milliseconds) for one `process_image` call.
/// `total_ms` is the whole Rust pipeline (prep → OCR → parse); it excludes the
/// image decode, which happens in the FFI seam before `process_image`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScanTimings {
    pub prep_ms: f64,
    pub detect_ms: f64,
    pub classify_ms: f64,
    pub recognize_ms: f64,
    pub parse_ms: f64,
    pub total_ms: f64,
}

/// Desktop/on-device prep constants (single source: `receipt-image`).
pub use receipt_image::{MAX_IMAGE_DIMENSION, OCR_IMAGE_PADDING};

/// Pre-OCR image prep via [`receipt_image`]: cap long side (Pillow int-truncation)
/// then white-pad. EXIF orientation is handled upstream by the capture layer
/// (iOS) or by `receipt_image::preprocess_image_bytes` (desktop bytes path).
pub fn resize_and_pad(img: &RgbImage) -> RgbImage {
    receipt_image::resize_and_pad(img, MAX_IMAGE_DIMENSION, OCR_IMAGE_PADDING)
}

/// Run the whole pipeline: image -> OCR -> parse/categorize/format.
///
/// `today` is `(year, month, day)` for date inference + the placeholder date.
#[allow(clippy::too_many_arguments)]
pub fn process_image(
    engine: &mut OcrEngine,
    img: &RgbImage,
    image_filename: &str,
    today: (i32, u32, u32),
    credit_card_account: &str,
    currency: &str,
    tax_account: &str,
    image_sha256: Option<&str>,
) -> ort::Result<ProcessedReceipt> {
    Ok(process_image_timed(
        engine,
        img,
        image_filename,
        today,
        credit_card_account,
        currency,
        tax_account,
        image_sha256,
    )?
    .0)
}

/// Like [`process_image`] but also returns per-stage [`ScanTimings`] for
/// on-device profiling.
#[allow(clippy::too_many_arguments)]
pub fn process_image_timed(
    engine: &mut OcrEngine,
    img: &RgbImage,
    image_filename: &str,
    today: (i32, u32, u32),
    credit_card_account: &str,
    currency: &str,
    tax_account: &str,
    image_sha256: Option<&str>,
) -> ort::Result<(ProcessedReceipt, ScanTimings)> {
    let t_all = Instant::now();

    let t = Instant::now();
    let prepared = resize_and_pad(img);
    let prep_ms = ms_since(t);

    let (detections, ocr) = engine.recognize_image_timed(&prepared)?;

    let raw: Vec<RawDetection> = detections
        .into_iter()
        .map(|d| RawDetection {
            points: d
                .points
                .iter()
                .map(|p| (p[0] as f64, p[1] as f64))
                .collect(),
            text: d.text,
            confidence: d.confidence as f64,
        })
        .collect();

    let t = Instant::now();
    let processed = process_receipt(
        raw,
        prepared.width() as i64,
        prepared.height() as i64,
        OCR_IMAGE_PADDING as i64,
        image_filename,
        None, // bundled default known-merchants
        today,
        credit_card_account,
        currency,
        tax_account,
        image_sha256,
    );
    let parse_ms = ms_since(t);

    let timings = ScanTimings {
        prep_ms,
        detect_ms: ocr.detect_ms,
        classify_ms: ocr.classify_ms,
        recognize_ms: ocr.recognize_ms,
        parse_ms,
        total_ms: ms_since(t_all),
    };
    Ok((processed, timings))
}

#[inline]
fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::OcrEngine;

    // Whole pipeline (image -> beancount), on-device-equivalent. Run with:
    //   cargo test -p ocr-paddle -- --ignored --nocapture
    #[test]
    #[ignore = "needs converted models + fixture"]
    fn process_image_end_to_end_costco() {
        let img = image::open("../../tests/receipts_e2e/costco_20260218_redact.jpg")
            .expect("load fixture")
            .to_rgb8();
        let mut engine = OcrEngine::from_paths(
            "../../models/PP-OCRv5_mobile_det.onnx",
            "../../models/PP-OCRv5_mobile_rec.onnx",
            Some("../../models/PP-LCNet_x1_0_textline_ori.onnx"),
        )
        .unwrap();

        let result = process_image(
            &mut engine,
            &img,
            "costco_20260218_redact",
            (2026, 2, 18),
            "Liabilities:CreditCard:PENDING",
            "CAD",
            "Expenses:Tax:HST",
            None,
        )
        .unwrap();

        let p = &result.parsed;
        eprintln!(
            "merchant={} date={:?} total={} tax={:?} subtotal={:?} items={}",
            p.merchant,
            p.date,
            p.total,
            p.tax,
            p.subtotal,
            p.items.len()
        );
        for it in &p.items {
            eprintln!("  {:>8}  {}  [{:?}]", it.price, it.description, it.category);
        }
        eprintln!("\n--- beancount ---\n{}", result.beancount);

        assert!(
            p.merchant.to_uppercase().contains("COSTCO"),
            "merchant: {}",
            p.merchant
        );
        assert_eq!(p.total, "221.97");
    }
}
