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

use receipt_core::date::Date;
use std::fmt;
use std::time::Instant;

use image::RgbImage;
use ocr_paddle::engine::OcrEngine;
use ocr_paddle::prep::resize_and_pad;
use receipt_core::ocr_transform::{RawDetection, RawDetectionPage, TransformError};
use receipt_core::process::{process_receipt_request, ProcessOptions, ProcessedReceipt};

// Re-exported so the FFI seam (and the harnesses) can depend on `scan` alone
// rather than reaching past it into `ocr-paddle`. Keeping the seam's dependency
// list to `scan` + `receipt-core` is what makes the composition root obvious.
pub use ocr_paddle::engine::OcrEngine as Engine;
pub use ocr_paddle::model_files;
pub use ocr_paddle::prep::{
    resize_and_pad as prepare_image, MAX_IMAGE_DIMENSION, OCR_IMAGE_PADDING,
};
// The overlay knobs are part of this crate's entry point, so callers configure a
// scan through `scan` alone rather than reaching into `receipt-core` for the
// options type and into here for the function that consumes it.
pub use receipt_core::process::ProcessOptions as ScanOptions;

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

/// Named receipt context shared with the parser.
pub use receipt_core::process::ProcessRequest as ScanRequest;

/// Why a composition failed.
///
/// Three genuinely different failures meet here: the OCR half can fail at the
/// model/runtime level, the detections and the image they were measured on can
/// fail to agree, and the parser half can reject the caller's rule overlays.
/// Keeping them separate lets the FFI seam map each to the error the apps
/// already distinguish, instead of flattening them to one string.
#[derive(Debug)]
pub enum ScanError {
    /// OCR could not run: model load, session, or inference failure.
    Ocr(ocr_paddle::Error),
    /// The engine's detections could not be reconciled with the padded image
    /// they were measured on. Unreachable in principle — this crate owns both
    /// sides — which is exactly why it is worth surfacing rather than
    /// unwrapping: it means prep and OCR have disagreed.
    Detections(TransformError),
    /// The caller's item-classifier overlay TOML was not valid.
    Rules(String),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::Ocr(e) => write!(f, "{e}"),
            ScanError::Detections(e) => write!(f, "{e}"),
            ScanError::Rules(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ScanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ScanError::Ocr(e) => Some(e),
            ScanError::Detections(e) => Some(e),
            ScanError::Rules(_) => None,
        }
    }
}

impl From<ocr_paddle::Error> for ScanError {
    fn from(e: ocr_paddle::Error) -> Self {
        ScanError::Ocr(e)
    }
}

impl From<TransformError> for ScanError {
    fn from(e: TransformError) -> Self {
        ScanError::Detections(e)
    }
}

/// Run the whole pipeline: image -> OCR -> parse/categorize/format.
///
/// `today` is `(year, month, day)` for date inference + the placeholder date.
#[allow(clippy::too_many_arguments)]
pub fn process_image(
    engine: &mut OcrEngine,
    img: &RgbImage,
    image_filename: &str,
    today: Date,
    credit_card_account: &str,
    currency: &str,
    tax_account: &str,
    image_sha256: Option<&str>,
) -> Result<ProcessedReceipt, ScanError> {
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
    today: Date,
    credit_card_account: &str,
    currency: &str,
    tax_account: &str,
    image_sha256: Option<&str>,
) -> Result<(ProcessedReceipt, ScanTimings), ScanError> {
    let request = ScanRequest {
        image_filename,
        today,
        credit_card_account,
        currency,
        tax_account,
        image_sha256,
    };
    // `ScanError::Rules` is unreachable through here — the only way to fail rule
    // loading is an overlay and the default options carry none — but it is no
    // longer worth a hand-written `unreachable!`: the signature already carries
    // `ScanError` for `Detections`, so passing every variant through costs
    // nothing and cannot turn a surprise into an abort inside a mobile app.
    process_image_with_options(engine, img, request, &ProcessOptions::default())
}

/// The composition, with rule/merchant overlays: **the one implementation** of
/// prep -> OCR -> parse.
///
/// [`process_image`] and [`process_image_timed`] are thin wrappers over this.
/// The FFI seam's overlay path used to be a second copy of this function; it
/// drifted no further than duplication before being folded back in, but the
/// layering test that forbids a second composition root can only read Cargo
/// manifests, so nothing caught it. Whatever is added here must stay here —
/// `device_sim` only reproduces device behaviour while there is one copy.
pub fn process_image_with_options(
    engine: &mut OcrEngine,
    img: &RgbImage,
    request: ScanRequest<'_>,
    options: &ProcessOptions,
) -> Result<(ProcessedReceipt, ScanTimings), ScanError> {
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

    // Validated here, once: the detections and the padded image they were
    // measured on stop being four loose values and become one page.
    let page = RawDetectionPage::try_new(
        raw,
        prepared.width() as i64,
        prepared.height() as i64,
        OCR_IMAGE_PADDING as i64,
    )?;

    let t = Instant::now();
    let processed = process_receipt_request(page, request, options).map_err(ScanError::Rules)?;
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
        let img = image::open("../receipt-core/tests/receipts_e2e/costco_20260218_redact.jpg")
            .expect("load fixture")
            .to_rgb8();
        let (det, rec, cls) = model_files::in_dir("../../models");
        let mut engine = OcrEngine::from_paths(det, rec, Some(cls)).unwrap();

        let result = process_image(
            &mut engine,
            &img,
            "costco_20260218_redact",
            Date::new(2026, 2, 18).unwrap(),
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
            eprintln!("  {:>8}  {}  [{:?}]", it.price, it.description, it.tag_path);
        }
        eprintln!("\n--- beancount ---\n{}", result.beancount);

        assert!(
            p.merchant.to_uppercase().contains("COSTCO"),
            "merchant: {}",
            p.merchant
        );
        assert_eq!(p.total.to_string(), "221.97");
    }
}
