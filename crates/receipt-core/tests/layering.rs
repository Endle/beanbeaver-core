//! The crate layering rules, as an executable assertion.
//!
//! `CLAUDE.md` states how the crates in this workspace may depend on each other.
//! A rule in prose gets re-litigated; a rule that fails `cargo test` does not.
//! Every assertion below names the failure it prevents.
//!
//! **Why this test lives in `receipt-core`** rather than somewhere more neutral:
//! `cargo test -p receipt-core` is the ORT-free fast gate — it needs no models,
//! no ONNX Runtime and no network. Putting the layering check anywhere further
//! down the graph would make the cheapest gate unable to catch the mistake that
//! matters most, which is this crate quietly acquiring a dependency.

use std::collections::BTreeSet;
use std::path::PathBuf;

use toml::Value;

fn crates_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is <workspace>/crates/receipt-core.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ dir")
        .to_path_buf()
}

fn manifest(crate_dir: &str) -> Value {
    let path = crates_dir().join(crate_dir).join("Cargo.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.parse::<Value>()
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// Dependency names in the given table (`dependencies` / `dev-dependencies` /
/// `build-dependencies`) of a crate's manifest.
fn deps(crate_dir: &str, table: &str) -> BTreeSet<String> {
    manifest(crate_dir)
        .get(table)
        .and_then(Value::as_table)
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default()
}

fn all_deps(crate_dir: &str) -> BTreeSet<String> {
    let mut set = deps(crate_dir, "dependencies");
    set.extend(deps(crate_dir, "dev-dependencies"));
    set.extend(deps(crate_dir, "build-dependencies"));
    set
}

/// Every crate directory in the workspace, so a newly added crate is covered by
/// the `ort` rule below without anyone remembering to update this list.
fn all_crate_dirs() -> Vec<String> {
    let mut dirs: Vec<String> = std::fs::read_dir(crates_dir())
        .expect("read crates/")
        .filter_map(Result::ok)
        .filter(|e| e.path().join("Cargo.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    dirs
}

/// receipt-core is the device-independent leaf: bbox -> itemized JSON. It must be
/// buildable and testable with no model, no ONNX Runtime, no image decoding and
/// no other workspace crate.
///
/// Prevents: the parser growing a dependency that makes `cargo test -p
/// receipt-core` need a native toolchain, which is what keeps the fast gate fast
/// and keeps the parser portable.
#[test]
fn receipt_core_is_a_device_independent_leaf() {
    let manifest = manifest("receipt-core");
    for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(t) = manifest.get(table).and_then(Value::as_table) else {
            continue;
        };
        for (name, spec) in t {
            assert!(
                spec.get("path").is_none(),
                "receipt-core must not depend on another workspace crate, but \
                 [{table}] has a path dependency on `{name}`. receipt-core is the \
                 leaf: everything else may depend on it, never the reverse."
            );
        }
    }

    let forbidden = ["ort", "ocr-paddle", "receipt-image", "scan", "image"];
    let present = all_deps("receipt-core");
    for name in forbidden {
        assert!(
            !present.contains(name),
            "receipt-core must not depend on `{name}` — it would make the pure \
             parser device-dependent."
        );
    }
}

/// ocr-paddle produces detections and stops. Composing them with the parser is
/// the `scan` crate's job.
///
/// Prevents: parsing behaviour drifting into the OCR engine, where it would be
/// untestable without models and invisible to the parser's own test corpus.
#[test]
fn ocr_paddle_does_not_depend_on_the_parser() {
    assert!(
        !all_deps("ocr-paddle").contains("receipt-core"),
        "ocr-paddle must not depend on receipt-core. It emits detections; the \
         `scan` crate joins them to the parser. If a whole-pipeline harness \
         needs both halves, it belongs in `scan`, not here."
    );
}

/// `ort` is what makes a build device-dependent: it links ONNX Runtime, needs a
/// platform-specific binary, and is the reason the mobile build scripts have
/// custom link plumbing. Confining it to one crate is what lets every other
/// crate stay trivially buildable.
///
/// Prevents: a second crate acquiring ORT and quietly doubling the number of
/// places that need the native toolchain (and, for F-Droid, need to be built
/// from source).
#[test]
fn only_ocr_paddle_links_onnx_runtime() {
    for dir in all_crate_dirs() {
        if dir == "ocr-paddle" {
            assert!(
                all_deps(&dir).contains("ort"),
                "ocr-paddle is supposed to be the crate that owns `ort`, but its \
                 manifest no longer lists it. If the engine moved, move this rule too."
            );
            continue;
        }
        assert!(
            !all_deps(&dir).contains("ort"),
            "`{dir}` depends on `ort`, but only ocr-paddle may. Use the types \
             ocr-paddle re-exports (`ocr_paddle::Result`) instead of taking a \
             direct dependency."
        );
    }
}

/// The FFI crate is a binding surface, not a second composition root. It reaches
/// the engine through `scan`, which re-exports the handful of items it needs.
///
/// Prevents: the seam re-assembling the pipeline itself, which would give the
/// apps a code path that `device_sim` does not exercise.
#[test]
fn ffi_composes_through_scan() {
    let ffi = deps("ffi", "dependencies");
    assert!(
        ffi.contains("scan"),
        "ffi must depend on `scan` — that is the composition root."
    );
    assert!(
        !ffi.contains("ocr-paddle"),
        "ffi must not depend on ocr-paddle directly; go through `scan` so there \
         is exactly one implementation of prep -> OCR -> parse."
    );
}
