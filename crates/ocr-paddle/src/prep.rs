//! Pre-OCR image preparation — the step that runs *before* detection, and the
//! reason the parser must later be told a padding value.
//!
//! This module used to also own the whole-pipeline composition
//! (`process_image`: prep → OCR → parse). That moved to the `scan` crate when
//! `ocr-paddle` stopped depending on `receipt-core`; see this repo's
//! `CLAUDE.md` for the layering rules. What is left here is genuinely part of
//! producing detections, so it belongs with the engine.

use image::RgbImage;

/// Desktop/on-device prep constants (single source: `receipt-image`).
pub use receipt_image::{MAX_IMAGE_DIMENSION, OCR_IMAGE_PADDING};

/// Pre-OCR image prep via [`receipt_image`]: cap long side (Pillow int-truncation)
/// then white-pad. EXIF orientation is handled upstream by the capture layer
/// (iOS) or by `receipt_image::preprocess_image_bytes` (desktop bytes path).
///
/// The white padding is why every consumer of the resulting detections must pass
/// [`OCR_IMAGE_PADDING`] (and the padded dimensions) to the parser: detections
/// come back in padded-image pixel coordinates.
pub fn resize_and_pad(img: &RgbImage) -> RgbImage {
    receipt_image::resize_and_pad(img, MAX_IMAGE_DIMENSION, OCR_IMAGE_PADDING)
}
