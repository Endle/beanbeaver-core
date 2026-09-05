# Detection Normalization Passes Plan

## Status

**Partly implemented.** The structural half landed in one change; the
observability half did not.

| Phase | State |
|---|---|
| 1. Freeze current behavior | Done, by measurement rather than by new snapshot tests — see Verification below. |
| 2. Validated detection pages | **Done.** `RawDetectionPage`, `DetectionPage`, `TransformError`. |
| 3. Extract the pass runner | **Done.** `NormalizationOptions::SHIPPING`, `normalize_detections`, `transform_with_options`. |
| 4. Per-pass tracing | **Not done.** Deferred — see below. |
| 5. `device_sim` ablation | **Not done.** Deferred — see below. |
| 6. Remove compatibility surfaces | **Done** — folded into phase 2. No wrapper was ever added; every caller moved in the same change. |

Phases 4 and 5 are deferred because the pass that has real decision logic
already reports it: `DeskewOutcome` carries estimator, angle, gate reason,
consensus, row tightening and sweep margins, and its doc comment exists to be
read when debugging a gate constant. The other three passes are index-keep and
reorder functions with unit tests; holding one out is now a struct literal
(`NormalizationOptions { deskew: false, ..SHIPPING }`) rather than an edit to
production code, which was the actual blocker. Add `PassReport` when a concrete
attribution question arrives that `DeskewOutcome` plus a temporary `eprintln!`
cannot answer.

Two corrections to the plan as written, both applied:

- **§1 asked for "at least four points per OCR polygon"** while `RawDetection`
  documented `>= 2` and direct Rust callers checked nothing, which would have
  been a silent behavioural change. Four is now enforced *deliberately*: it is
  the rule the FFI seam already applied, `ocr_paddle::engine::Detection` is
  `[[f32; 2]; 4]`, and all 12,279 detections in the public and private cached
  corpora are 4-point. Confidence is checked for finiteness only, as the plan
  itself proposed.
- **The acceptance criterion about documenting CPU as the execution provider
  belongs to a different change** and has been dropped from this one. The EP
  decision is settled and separate: iOS, Android, CI and `device_sim` all use
  ONNX Runtime's plain CPU EP (MLAS), with CoreML and XNNPACK dormant
  experimental features. Detection normalization begins after the OCR engine has
  produced text boxes, whichever provider produced them.

One deviation from §3: `transform` and `transform_with_options` return
`OcrDocument`, not `Result<_, TransformError>`. All validation happens in
`RawDetectionPage::try_new`, so a `Result` on the transform would be an error
type no code path can produce.

## Motivation

PaddleOCR produces recognized text with polygon bounding boxes in padded-image
pixel coordinates. Before parsing, `receipt-core` applies several improvements
to that detection list:

1. Drop low-quality detections, with the existing priced-row exception.
2. Drop overlapping Costco Bottom-Of-Basket marker detections.
3. Correct supported page tilt by shearing detection geometry.
4. Sort detections into stable reading order.

These operations already behave like compiler passes: each consumes and
produces the same conceptual detection representation, each has independent
tests, their order matters, and the output is lowered into a different
representation only after they finish. However, orchestration is currently a
private, hard-coded function in `ocr_transform.rs`. A developer cannot disable
one pass while holding OCR output constant, and there is no common trace showing
what each pass changed.

That makes attribution unnecessarily difficult. When a cached detection list
parses differently after a bbox change, we should be able to answer whether the
change came from filtering, deskewing, ordering, or line grouping without
editing production code.

## Scope

This plan covers the detection-preserving portion of the parser input pipeline:

```text
RawDetectionPage (padded pixels)
    |
    | mandatory validation and de-padding
    v
DetectionPage (de-padded pixels)
    |
    | low-quality filter
    | BOB-marker filter
    | deskew
    | reading-order sort
    v
DetectionPage (normalized and ordered)
    |
    | mandatory line grouping and coordinate normalization
    v
OcrDocument ([0, 1] coordinates)
```

The four middle operations are the configurable passes. Validation,
de-padding, line grouping, and construction of `OcrDocument` are representation
boundaries, not optional passes.

## Non-goals

- Do not change OCR models, ONNX Runtime, or execution providers.
- Do not make the shipping pipeline configurable through environment variables.
- Do not expose pass controls through UniFFI or app settings.
- Do not allow arbitrary pass reordering in the first version.
- Do not combine text extraction, spatial extraction, or field extraction with
  bbox normalization passes; they operate on a later representation.
- Do not retune thresholds or alter pass behavior as part of the structural
  migration.

## Design

### 1. Make the coordinate-space contract a type

Replace the independently supplied detection list, width, height, and padding
with a validated input value:

```rust
pub struct RawDetectionPage {
    pub detections: Vec<RawDetection>,
    pub padded_width: u32,
    pub padded_height: u32,
    pub padding: u32,
}

impl RawDetectionPage {
    pub fn try_new(
        detections: Vec<RawDetection>,
        padded_width: i64,
        padded_height: i64,
        padding: i64,
    ) -> Result<Self, TransformError>;
}
```

Construction validates:

- positive dimensions;
- `2 * padding < width` and `2 * padding < height`;
- at least four points per OCR polygon;
- finite point coordinates and confidence values;
- confidence within the range accepted by the existing pipeline.

The exact confidence policy must preserve current OCR behavior. If out-of-range
confidence is currently tolerated, validation should initially reject only
non-finite values and record tighter validation as separate behavioral work.

After validation and de-padding, the internal `DetectionPage` owns the image
dimensions and `Vec<Detection>` together. A pass cannot accidentally receive
boxes from one image and dimensions from another.

### 2. Use a fixed-order, explicitly enabled pipeline

The initial pass manager should be a concrete runner, not a dynamic plugin
system:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectionPass {
    LowQuality,
    BobMarkers,
    Deskew,
    ReadingOrder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NormalizationOptions {
    pub filter_low_quality: bool,
    pub filter_bob_markers: bool,
    pub deskew: bool,
    pub sort_reading_order: bool,
}

impl NormalizationOptions {
    pub const SHIPPING: Self = Self {
        filter_low_quality: true,
        filter_bob_markers: true,
        deskew: true,
        sort_reading_order: true,
    };
}
```

The runner always considers passes in the current production order. Options can
skip a pass but cannot reorder them. This preserves the dependency between
passes and prevents a diagnostic profile from accidentally becoming a second
production pipeline.

The existing pass implementations should remain ordinary functions with focused
unit tests. The runner adapts their current outputs—kept indices, corrected
coordinates, or sorted indices—into mutations of its owned `DetectionPage`.
A trait-object-based `DetectionPass` abstraction is unnecessary while the pass
set is small and closed.

### 3. Preserve one unconfigurable production entry point

The existing `transform` function remains the normal API and always selects the
shipping profile:

```rust
pub fn transform(page: RawDetectionPage) -> Result<OcrDocument, TransformError> {
    transform_with_options(page, NormalizationOptions::SHIPPING)
        .map(|result| result.document)
}
```

An explicitly named diagnostic API accepts another profile:

```rust
pub fn transform_with_options(
    page: RawDetectionPage,
    options: NormalizationOptions,
) -> Result<NormalizationResult, TransformError>;
```

`scan::process_image` and the UniFFI scanning path must continue to call only
the production entry point. Pass options are for tests and diagnostics; they do
not become part of `ParseOptions` or the mobile contract.

If `device_sim` needs whole-receipt scoring with a custom profile, add a clearly
diagnostic `scan` entry point rather than teaching FFI or the shipping scan
request about pass controls.

### 4. Record a cheap per-pass trace

The diagnostic result reports what the pipeline did:

```rust
pub struct NormalizationResult {
    pub document: OcrDocument,
    pub reports: Vec<PassReport>,
}

pub struct PassReport {
    pub pass: DetectionPass,
    pub enabled: bool,
    pub input_count: usize,
    pub output_count: usize,
    pub changed_boxes: usize,
    pub detail: PassDetail,
}

pub enum PassDetail {
    None,
    Filtered { removed_indices: Vec<usize> },
    Deskew(DeskewReport),
    Reordered { moved: usize },
}
```

`DeskewReport` should expose the stable diagnostic portion of the existing
`DeskewOutcome`: estimator, proposed angle, whether it applied, gate reason,
and evidence counts. It should not require retaining a full bbox snapshot.

Production `transform` may discard reports. If report allocation is measurable,
the runner can accept a trace level or sink later; avoid adding that complexity
until measurement shows it is needed.

### 5. Add pass ablation to `device_sim`

Add two diagnostic forms:

```text
device_sim <path> --cached --disable-pass deskew
device_sim <path> --cached --ablate-passes
```

`--disable-pass` runs one explicit profile. `--ablate-passes` runs:

```text
shipping
shipping minus low-quality
shipping minus BOB markers
shipping minus deskew
shipping minus reading-order
```

Cached `.ocr.json` must be the default input for ablation. It holds model output
constant, so score changes are attributable to normalization rather than OCR
nondeterminism. Live OCR may be supported when requested explicitly, but each
live image should be recognized once and the same resulting detections replayed
through every profile.

Report at least:

- merchant/date/total matches;
- critical-item matches;
- parsed item count;
- warning counts by kind;
- number of receipts whose structured result changed;
- per-pass counts and deskew gate reasons.

The output must name the disabled pass and input source so an ablation scorecard
is self-describing.

## Migration plan

### Phase 1: Freeze current behavior

1. Add a direct test that the current hard-coded normalization order produces
   the checked-in public cached E2E results.
2. Add focused pipeline-order tests using small synthetic detection pages.
3. Record the current public and private cached-corpus scorecards.
4. Do not change any threshold, regex, or geometric rule in this phase.

### Phase 2: Introduce validated detection pages

1. Add `RawDetectionPage`, `DetectionPage`, and `TransformError`.
2. Convert `ocr_transform::transform` internally while retaining a temporary
   compatibility wrapper for existing callers.
3. Migrate `process`, `scan`, FFI `parse_detections`, cached E2E harnesses, and
   diagnostics to construct the page explicitly.
4. Remove the positional width/height/padding entry point after all callers move.

This phase should also eliminate the current difference where FFI validates
polygon length but direct Rust callers do not.

### Phase 3: Extract the pass runner

1. Add `NormalizationOptions::SHIPPING`.
2. Move the existing orchestration from `ocr_transform.rs` into a normalization
   pipeline module without changing the pass functions.
3. Make `transform` use `SHIPPING` exclusively.
4. Add `transform_with_options` for diagnostics and tests.
5. Prove that all-enabled output is identical to the pre-refactor output.

### Phase 4: Add tracing

1. Introduce `DetectionPass`, `PassReport`, and stable deskew diagnostics.
2. Test report counts against actual input/output lengths.
3. Ensure a disabled pass appears as disabled rather than disappearing from the
   trace; a complete trace makes profile comparison easier.
4. Keep filesystem dumping and presentation outside `receipt-core`.

### Phase 5: Add `device_sim` ablation

1. Parse pass names strictly and reject unknown names.
2. Recognize each live image no more than once per ablation run.
3. Reuse one scoring implementation for baseline and pass-disabled profiles.
4. Print per-receipt changes under a verbose flag and aggregate results by
   default.
5. Document the command in `device_sim --help` and the architecture guide.

### Phase 6: Remove compatibility surfaces

1. Remove the old positional transform API.
2. Narrow normalization internals that diagnostics no longer need directly.
3. Replace source-text assertions with type/API boundaries where the new runner
   makes that possible.
4. Update module documentation to name the new input, pass, and lowering stages.

## Verification

Every phase must run the smallest applicable gate, followed by the full cached
corpus before merge:

```bash
cargo fmt --check
cargo clippy -p receipt-core --all-targets -- -D warnings
cargo test -p receipt-core --lib --test layering
cargo test -p receipt-core --test public_e2e -- --nocapture
```

When the private corpus is available:

```bash
BEANBEAVER_PRIVATE_TESTS_DIR=<path> \
  cargo test --release -p receipt-core --test private_e2e -- --nocapture
```

Before removing compatibility wrappers, also run the workspace lint gate and
the model-backed `scan` tests on a supported host.

## Acceptance criteria

The work is complete when:

- production scan output is unchanged with all passes enabled;
- the shipping profile is explicit and is the only profile reachable through
  UniFFI scanning;
- invalid dimensions and polygons fail at detection-page construction;
- every normalization pass can be disabled independently in tests and
  `device_sim`;
- pass order remains fixed;
- an ablation run reuses the same OCR detections for every profile;
- traces explain filtering, reordering, and deskew decisions without filesystem
  I/O in `receipt-core`;
- public and private cached-corpus results show no unreviewed regression;
- documentation consistently records CPU as the execution provider used by
  both shipping apps.

## Follow-up work

Once this structure is established, proposed bbox improvements should land as
individual passes or deliberate changes to one existing pass. Each change should
include:

1. a focused unit test;
2. an all-passes corpus comparison;
3. a pass-disabled ablation showing the change is attributable to that pass;
4. updated trace fields when a new decision needs to be explainable.

Threshold tuning and new pass behavior should be separate commits from the
structural migration so corpus changes cannot hide inside code movement.
