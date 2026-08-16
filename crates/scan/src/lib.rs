//! Composition: a decoded image -> structured receipt + beancount, on-device.
//!
//! This crate exists to be **the one place** the two halves of the pipeline meet:
//!
//! ```text
//!   ocr-paddle    pixels -> detections     (device-dependent: links ONNX via `ort`)
//!   receipt-core  detections -> receipt    (device-independent: pure Rust)
//! ```
//!
//! Neither half knows about the other — see the layering rules in the repo's
//! `CLAUDE.md`. `ocr-paddle` deliberately does **not** depend on `receipt-core`,
//! so the OCR engine cannot quietly grow parsing behaviour, and `receipt-core`
//! stays buildable (and testable) with no model, no ONNX, and no image decoding.
//! Joining them is this crate's whole job.
//!
//! Everything that needs *both* halves lives here too: `examples/device_sim.rs`
//! and the live end-to-end tests. That is deliberate. `device_sim`'s value is
//! that it runs the exact code path a phone runs, so it must call
//! [`process_image`] rather than re-implement the composition — a second copy
//! could drift and the simulator would stop simulating.

use std::time::Instant;

use image::RgbImage;
use ocr_paddle::engine::OcrEngine;
use ocr_paddle::prep::resize_and_pad;
// `ocr_paddle::Result` rather than `ort::Result`: only `ocr-paddle` may depend on
// `ort`, so it re-exports the type for composition layers like this one.
use ocr_paddle::Result as OcrResult;
use receipt_core::ocr_transform::RawDetection;
use receipt_core::process::{process_receipt, ProcessedReceipt};

// Re-exported so the FFI seam (and the harnesses) can depend on `scan` alone
// rather than reaching past it into `ocr-paddle`. Keeping the seam's dependency
// list to `scan` + `receipt-core` is what makes the composition root obvious.
pub use ocr_paddle::engine::OcrEngine as Engine;
pub use ocr_paddle::model_files;
pub use ocr_paddle::prep::{
    resize_and_pad as prepare_image, MAX_IMAGE_DIMENSION, OCR_IMAGE_PADDING,
};

/// End-to-end per-stage timings (milliseconds) for one [`process_image`] call.
/// `total_ms` is the whole Rust pipeline (prep → OCR → parse); it excludes the
/// image decode, which happens in the FFI seam before `process_image`.
///
/// This type lives here rather than in `ocr-paddle` because it spans both
/// halves: `parse_ms` and `total_ms` are only measurable at the composition.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScanTimings {
    pub prep_ms: f64,
    pub detect_ms: f64,
    pub classify_ms: f64,
    pub recognize_ms: f64,
    pub parse_ms: f64,
    pub total_ms: f64,
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
) -> OcrResult<ProcessedReceipt> {
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
) -> OcrResult<(ProcessedReceipt, ScanTimings)> {
    let t_all = Instant::now();

    let t = Instant::now();
    let prepared = resize_and_pad(img);
    let prep_ms = ms_since(t);

    let (detections, ocr) = engine.recognize_image_timed(&prepared)?;

    // The contract between the two halves is a coordinate space as much as a
    // type: detections are in *padded-image* pixels, so the parser is handed the
    // padded dimensions and the padding it must undo. Whoever composes owns
    // keeping these consistent with the `resize_and_pad` that actually ran —
    // which is the main reason the composition is one function and not inlined
    // at each call site.
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

    // Whole pipeline (image -> beancount), on-device-equivalent. Run with:
    //   cargo test -p scan -- --ignored --nocapture
    #[test]
    #[ignore = "needs converted models + fixture"]
    fn process_image_end_to_end_costco() {
        let img = image::open("../../tests/receipts_e2e/costco_20260218_redact.jpg")
            .expect("load fixture")
            .to_rgb8();
        let (det, rec, cls) = model_files::in_dir("../../models");
        let mut engine = OcrEngine::from_paths(det, rec, Some(cls)).unwrap();

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
