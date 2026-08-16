//! The canonical on-disk filenames of the three converted PP-OCRv5 models.
//!
//! One definition, because the alternative was five: the FFI seam and four test
//! harnesses each spelled these out, so swapping a model meant editing all five
//! and hoping the grep found them. The names live *here* — with the engine that
//! loads them — and `scan` re-exports the module so the seam can reach it
//! without depending on this crate directly (see `scan`'s re-export block).
//!
//! **These are exact names on purpose.** `device_sim` deliberately resolves by
//! `_det/_rec/_ori` suffix instead, so `--models DIR` can point at an
//! experimental set; that tolerance is right for a diagnostic and wrong for the
//! shipping path, where two matching files in one directory would silently load
//! whichever the directory iteration happened to yield first. Here, a missing
//! or renamed model fails loudly with the path it looked for.
//!
//! The weights themselves are not vendored — see `docs/ocr-models.md`.

use std::path::{Path, PathBuf};

/// PP-OCRv5 mobile text detection (DB).
pub const DET: &str = "PP-OCRv5_mobile_det.onnx";
/// PP-OCRv5 mobile text recognition, English 436-class head.
pub const REC: &str = "PP-OCRv5_mobile_rec.onnx";
/// PP-LCNet textline-orientation classifier (0°/180°), optional at session load.
pub const CLS: &str = "PP-LCNet_x0_25_textline_ori.onnx";

/// The three model paths inside `dir`, in [`OcrEngine::from_paths`] argument
/// order.
///
/// [`OcrEngine::from_paths`]: crate::engine::OcrEngine::from_paths
pub fn in_dir(dir: impl AsRef<Path>) -> (PathBuf, PathBuf, PathBuf) {
    let dir = dir.as_ref();
    (dir.join(DET), dir.join(REC), dir.join(CLS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_carries_the_suffix_device_sim_resolves_by() {
        // device_sim finds models by these suffixes; if a rename ever broke the
        // convention the two resolution paths would silently disagree.
        assert!(DET.ends_with("_det.onnx"));
        assert!(REC.ends_with("_rec.onnx"));
        assert!(CLS.ends_with("_ori.onnx"));
    }

    #[test]
    fn in_dir_joins_all_three() {
        let (det, rec, cls) = in_dir("/m");
        assert_eq!(det, Path::new("/m").join(DET));
        assert_eq!(rec, Path::new("/m").join(REC));
        assert_eq!(cls, Path::new("/m").join(CLS));
    }
}
