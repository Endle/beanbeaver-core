//! On-device PP-OCRv5 OCR pipeline (the "fat Rust" seam): image pixels ->
//! detections (`[bbox, [text, conf]]`), matching the desktop `beanbeaver-ocr`
//! service so downstream `receipt-core` parsing behaves the same.
//!
//! Stages (built incrementally):
//! - [`preprocess`] — detection input tensor (resize_long 960 / pad-32 / normalize).
//! - `db_postprocess` — DB probability map -> quad boxes (next).
//! - recognition + CTC decode, textline-orientation cls, and `ort` inference wiring.
//!
//! **Layering:** this crate stops at detections. It must not depend on
//! `receipt-core` — joining OCR output to the parser is the `scan` crate's job.
//! See the repo `CLAUDE.md`.

pub mod classify;
pub mod db_postprocess;
pub mod detect;
pub mod engine;
pub mod model_files;
pub mod prep;
pub mod preprocess;
pub mod recognize;
pub(crate) mod session;

/// The engine's error and result types, re-exported.
///
/// This crate is the only one in the workspace allowed to depend on `ort` (it is
/// what makes the build device-dependent). Re-exporting these lets callers name
/// the fallible result of an OCR call — `ocr_paddle::Result<T>` — without taking
/// an `ort` dependency of their own, so the rule stays enforceable.
pub use ort::{Error, Result};
