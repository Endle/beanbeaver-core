//! One-off helper (not a gate): regenerate cached `.ocr.json` snapshots for the
//! vendored corpus by running the real det/rec/cls pipeline on the `.jpg`s.
//!
//!   cargo test -p ocr-paddle --test gen_ocr_snapshot -- --ignored --nocapture
//!
//! Emits `<stem>.ocr.json` next to each `<stem>.jpg` that lacks one, in the exact
//! shape the cached harness consumes (padded-image coordinate space, matching
//! `scan::process_image`'s `resize_and_pad` -> `recognize_image_timed`). Ignored so it
//! never runs in normal `cargo test`.
//!
//! Target corpus: the vendored public fixtures by default, or the out-of-tree
//! private corpus when `BEANBEAVER_PRIVATE_TESTS_DIR` is set (same env var
//! `receipt-core`'s `private_e2e` reads, so one export drives both). Subdirectories
//! are walked, which the private corpus needs — it partitions fixtures by merchant.

use std::fs;
use std::path::{Path, PathBuf};

use ocr_paddle::engine::OcrEngine;
use ocr_paddle::prep::resize_and_pad;
use serde_json::{json, Value};

fn manifest_rel(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Recursively collect `*.jpg` under `dir`.
fn collect_jpgs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jpgs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jpg") {
            out.push(path);
        }
    }
}

#[test]
#[ignore = "generator, not a gate: run with --ignored to (re)build .ocr.json snapshots"]
fn gen_ocr_snapshot() {
    let fixtures = match std::env::var_os("BEANBEAVER_PRIVATE_TESTS_DIR") {
        Some(root) => PathBuf::from(root).join("receipts_e2e"),
        None => manifest_rel("../receipt-core/tests/receipts_e2e"),
    };
    assert!(
        fixtures.is_dir(),
        "no fixtures dir at {}",
        fixtures.display()
    );
    let models = manifest_rel("../../models");
    let (det, rec, cls) = (
        models.join("PP-OCRv5_mobile_det.onnx"),
        models.join("PP-OCRv5_mobile_rec.onnx"),
        models.join("PP-LCNet_x1_0_textline_ori.onnx"),
    );
    assert!(
        det.exists() && rec.exists() && cls.exists(),
        "OCR models missing under {}",
        models.display()
    );

    // Only stems named on the command line (after `--`), else every .jpg without
    // a sibling .ocr.json. Filter out the harness flags cargo passes through.
    let wanted: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with("--") && a != "gen_ocr_snapshot")
        .collect();

    let mut engine = OcrEngine::from_paths(&det, &rec, Some(&cls)).expect("load PP-OCRv5 models");

    let mut jpgs = Vec::new();
    collect_jpgs(&fixtures, &mut jpgs);
    jpgs.sort();

    let mut made = 0;
    for path in jpgs {
        let Some(stem) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".jpg"))
        else {
            continue;
        };
        let stem = stem.to_string();
        if !wanted.is_empty() && !wanted.iter().any(|w| stem.contains(w.as_str())) {
            continue;
        }
        let out = path.with_file_name(format!("{stem}.ocr.json"));
        if wanted.is_empty() && out.exists() {
            continue; // don't clobber an existing snapshot in bulk mode
        }

        let img = image::open(&path)
            .unwrap_or_else(|e| panic!("decode {stem}.jpg: {e}"))
            .to_rgb8();
        let prepared = resize_and_pad(&img);
        let (detections, _) = engine.recognize_image_timed(&prepared).expect("ocr");

        let dets: Vec<Value> = detections
            .into_iter()
            .map(|d| {
                let pts: Vec<Value> = d
                    .points
                    .iter()
                    .map(|p| json!([p[0].round() as i64, p[1].round() as i64]))
                    .collect();
                json!([pts, [d.text, d.confidence]])
            })
            .collect();

        let snapshot = json!({
            "status": "success",
            "image_width": prepared.width(),
            "image_height": prepared.height(),
            "detections": dets,
        });
        fs::write(
            &out,
            format!("{}\n", serde_json::to_string_pretty(&snapshot).unwrap()),
        )
        .unwrap();
        eprintln!(
            "wrote {} ({} detections)",
            out.display(),
            snapshot["detections"].as_array().unwrap().len()
        );
        made += 1;
    }
    eprintln!("gen_ocr_snapshot: wrote {made} snapshot(s)");
}
