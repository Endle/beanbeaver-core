//! UniFFI seam between Swift and the Rust receipt core.
//!
//! Swift loads the OCR models once (`OcrSession::new`) and then calls
//! `scan` per captured image, handing in encoded JPEG/PNG bytes and getting
//! back a structured receipt + beancount fragment. Everything heavy (ONNX
//! inference, OCR post-processing, parsing, categorizing, formatting) runs
//! here in Rust — the "fat-Rust" seam from `docs/ios_port.md`.
//!
//! Additional entry points (no models required for pure parse/reformat):
//! - [`parse_detections`] — OCR detections → receipt (swap OCR backends / re-parse)
//! - [`reformat_receipt`] — apply user corrections → new beancount
//! - rule overlays via [`ParseOptions`]

use std::fmt;
use std::sync::{Arc, Mutex};

use receipt_core::merchant_match::{
    MerchantMatch as CoreMerchantMatch, MerchantMatchStatus as CoreStatus,
};
use receipt_core::ocr_transform::RawDetection;
use receipt_core::process::{
    process_receipt_with_options, reformat_parsed_receipt, FieldConfidence, ProcessOptions,
    ProcessedReceipt, ReceiptCorrections,
};
use receipt_core::receipt_common::ReceiptWarningKind as CoreWarningKind;
use receipt_core::receipt_parser::{
    ParsedReceiptData, ParsedReceiptItem, ParsedReceiptTender, ParsedReceiptWarning,
};
use receipt_core::rules::RuleBook as CoreRuleBook;
use scan::{process_image_timed, Engine as OcrEngine, ScanTimings as CoreScanTimings};

uniffi::setup_scaffolding!();

/// Fixed bundle filenames for the three converted PP-OCRv5 models. The Swift
/// app ships these as resources; `OcrSession::new` is handed their directory.
const DET_MODEL: &str = "PP-OCRv5_mobile_det.onnx";
const REC_MODEL: &str = "PP-OCRv5_mobile_rec.onnx";
const CLS_MODEL: &str = "PP-LCNet_x1_0_textline_ori.onnx";

/// Calendar date passed in from Swift (used for date inference + placeholder).
#[derive(uniffi::Record)]
pub struct DateYmd {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

/// One parsed line item.
#[derive(uniffi::Record)]
pub struct ReceiptItem {
    pub description: String,
    pub price: String,
    pub quantity: i32,
    /// The beancount account this line posts to, already resolved. Was
    /// `category`, which held a classifier key that was *not necessarily* an
    /// account — callers could not tell which they had been given.
    pub account: Option<String>,
    /// This line's classification, one entry per node from least to most
    /// specific: `[{grocery, "Grocery"}, {grocery/dairy, "Dairy"}]`.
    ///
    /// Each entry carries an authored display name, so a consumer never has to
    /// invent one from the path. Deriving it by capitalizing the segment is what
    /// rendered `energy_drink` as "Energy_drink". Empty when no rule matched.
    pub tags: Vec<ItemTag>,
}

/// One node of an item's tag path, with the name to show for it.
#[derive(uniffi::Record, Clone, Debug)]
pub struct ItemTag {
    /// Full path (`grocery/dairy`), stable across releases — the identifier to
    /// match on.
    pub path: String,
    /// Authored label ("Dairy"). Presentation only; never match on it.
    pub display: String,
}

/// One payment tender (split tender / multi-payment receipts).
#[derive(uniffi::Record)]
pub struct ReceiptTender {
    pub amount: String,
    pub account: Option<String>,
    pub kind: String,
    pub raw_label: String,
}

/// One pipeline phase — the single source of truth for phase *names*, shared by
/// every consumer. The Rust core fills the on-device phases (`Decode`…`Parse`);
/// each app appends its own UI-side phases (`Acquire`, `Encode`, `Render`) using
/// these same variants. Because UniFFI generates one enum for Swift and Kotlin,
/// the phase names match across platforms by construction — they can't drift.
///
/// Ordered by pipeline position. Adding a finer phase later (e.g. splitting
/// `Detect` into infer/postprocess) is one new variant — no record-shape break.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// App-side: capture / pick the image into memory. Filled by the app.
    Acquire,
    /// App-side: encode/normalize to the JPEG/PNG bytes handed to `scan`. App-filled.
    Encode,
    /// Core: decode the received JPEG/PNG bytes into pixels.
    Decode,
    /// Core: resize + white-pad for OCR.
    Prep,
    /// Core: text-box detection (DB).
    Detect,
    /// Core: textline-orientation classification.
    Classify,
    /// Core: text recognition.
    Recognize,
    /// Core: detections → itemized receipt + beancount.
    Parse,
    /// App-side: build/display the result UI. Filled by the app.
    Render,
}

/// One measured phase. `ScanTimings` is just an ordered list of these, so the
/// debug view can iterate generically and new phases render with no UI change.
#[derive(uniffi::Record, Clone, Debug)]
pub struct PhaseSpan {
    pub phase: Phase,
    pub ms: f64,
}

/// Per-phase on-device timings (milliseconds) for one scan, surfaced behind the
/// app's debug toggle for profiling. The total is the sum of `spans`; the app's
/// own wall-clock is measured separately. Empty for the model-free
/// `parse_detections` path.
#[derive(uniffi::Record, Clone, Debug, Default)]
pub struct ScanTimings {
    pub spans: Vec<PhaseSpan>,
}

impl ScanTimings {
    /// Build the core (on-device) spans from the OCR pipeline's stage timings
    /// plus the FFI-measured image `decode` step. App-side spans (`Acquire`,
    /// `Encode`, `Render`) are appended by the caller on each platform.
    fn from_core(t: CoreScanTimings, decode_ms: f64) -> Self {
        Self {
            spans: vec![
                PhaseSpan {
                    phase: Phase::Decode,
                    ms: decode_ms,
                },
                PhaseSpan {
                    phase: Phase::Prep,
                    ms: t.prep_ms,
                },
                PhaseSpan {
                    phase: Phase::Detect,
                    ms: t.detect_ms,
                },
                PhaseSpan {
                    phase: Phase::Classify,
                    ms: t.classify_ms,
                },
                PhaseSpan {
                    phase: Phase::Recognize,
                    ms: t.recognize_ms,
                },
                PhaseSpan {
                    phase: Phase::Parse,
                    ms: t.parse_ms,
                },
            ],
        }
    }
}

/// How much to trust `MerchantMatch::canonical`. Mirrors
/// `receipt_core::merchant_match::MerchantMatchStatus`.
#[derive(uniffi::Enum)]
pub enum MerchantMatchStatus {
    /// The raw OCR text already contains a known merchant verbatim.
    Exact,
    /// Confidently normalized to `canonical`; safe to show in place of `raw`.
    Corrected,
    /// A plausible `canonical`, but not corroborated — offer it (e.g. in grey)
    /// without replacing `raw`.
    Suggested,
    /// No family matched; only `raw` is meaningful.
    Unknown,
}

/// Merchant identity resolution surfaced to Swift. The `merchant` field on
/// `ReceiptResult` is the display string already chosen from this
/// (`canonical` when `Exact`/`Corrected`, else `raw`); this record lets the UI
/// show the correction — e.g. render `raw` in grey under a `Suggested`
/// `canonical` — instead of silently trusting a low-confidence guess.
#[derive(uniffi::Record)]
pub struct MerchantMatch {
    /// Exactly what OCR produced for the merchant header.
    pub raw: String,
    /// Canonical family name, when one was matched.
    pub canonical: Option<String>,
    pub status: MerchantMatchStatus,
    /// Similarity of the chosen family in `[0, 1]` (diagnostics/UI only).
    pub score: f64,
}

impl From<CoreMerchantMatch> for MerchantMatch {
    fn from(m: CoreMerchantMatch) -> Self {
        let status = match m.status {
            CoreStatus::Exact => MerchantMatchStatus::Exact,
            CoreStatus::Corrected => MerchantMatchStatus::Corrected,
            CoreStatus::Suggested => MerchantMatchStatus::Suggested,
            CoreStatus::Unknown => MerchantMatchStatus::Unknown,
        };
        Self {
            raw: m.raw,
            canonical: m.canonical,
            status,
            score: m.score,
        }
    }
}

/// Heuristic per-field trust for "needs review" UI (`[0, 1]` scores).
#[derive(uniffi::Record)]
pub struct FieldConfidences {
    pub merchant: f64,
    pub date: f64,
    pub total: f64,
    pub items_categorized: f64,
    pub needs_review: bool,
}

impl From<FieldConfidence> for FieldConfidences {
    fn from(c: FieldConfidence) -> Self {
        Self {
            merchant: c.merchant,
            date: c.date,
            total: c.total,
            items_categorized: c.items_categorized,
            needs_review: c.needs_review,
        }
    }
}

/// What a parser finding *is*, mirroring
/// [`receipt_core::receipt_common::ReceiptWarningKind`] one-for-one.
///
/// Carries no severity on purpose: the parser reports, the client ranks. A
/// client is expected to `switch` with a fallback arm — variants are additive
/// over time, and an unrecognized kind should degrade to "show it quietly",
/// never to a crash.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReceiptWarningKind {
    /// Items (+ tax) overshoot the printed total: the entry cannot balance.
    TotalMismatch,
    /// Items don't sum to the printed subtotal: a line was missed or doubled.
    SubtotalMismatch,
    /// A price with no description to attach it to.
    PossibleMissedItem,
    /// A malformed OCR price was repaired against the summary amounts.
    PriceAutoCorrected,
    /// A price was discarded for exceeding the receipt total.
    DroppedImplausiblePrice,
    /// An item matched no classifier rule, so it has no tags and no account.
    UncategorizedItem,
    /// The printed tender lines don't add up to the printed total. One of the
    /// two is misread and the arithmetic can't say which, so nothing is
    /// repaired; the beancount falls back to a single payment posting.
    TenderMismatch,
}

/// One parser finding: what it is, what it says, and which item it sits under.
#[derive(uniffi::Record, Clone)]
pub struct ReceiptWarning {
    pub kind: ReceiptWarningKind,
    /// Human-readable detail. For display only — never switch on this text;
    /// that is what `kind` is for.
    pub message: String,
    /// Item index this warning follows, or `-1` when it is about the receipt
    /// as a whole.
    pub after_item_index: i32,
}

/// Flattened, Swift-friendly view of `ProcessedReceipt`.
#[derive(uniffi::Record)]
pub struct ReceiptResult {
    /// Display merchant name (`merchant_match`'s chosen display string).
    pub merchant: String,
    /// Full merchant resolution (raw OCR, canonical family, confidence).
    pub merchant_match: MerchantMatch,
    /// ISO `YYYY-MM-DD`, or `None` if the parser found no date.
    pub date: Option<String>,
    pub date_is_placeholder: bool,
    pub total: String,
    pub tax: Option<String>,
    pub subtotal: Option<String>,
    pub items: Vec<ReceiptItem>,
    /// Every finding the parser made, in kind form. Ranking them into badges,
    /// colors, or silence is the client's call — see [`ReceiptWarningKind`].
    pub warnings: Vec<ReceiptWarning>,
    /// OCR dump used for card-last4 / metadata in beancount. Round-trip this
    /// through [`reformat_receipt`] so edits do not drop payment metadata.
    pub raw_text: String,
    /// Image filename embedded in parse/format (defaults to `receipt.jpg` on scan).
    pub image_filename: String,
    /// Split-tender payment lines; empty means single default liability posting.
    pub tenders: Vec<ReceiptTender>,
    pub beancount: String,
    /// Greppable identity embedded in `beancount` (`bb-<yyyymmdd>-<sha8>`), or
    /// `None` if the image hash could not be computed.
    pub beanbeaver_id: Option<String>,
    /// Path the receipt image should be saved under, relative to the ledger's
    /// documents root (`beanbeaver/<name>.jpg`) — exactly the value written into
    /// the `document:` metadata. Save the scanned JPEG here so the link resolves.
    pub document_relpath: Option<String>,
    /// Per-stage timings for this scan (on-device profiling). Zero for parse-only.
    pub timings: ScanTimings,
    /// Field-level confidence / needs-review hint for the UI.
    pub confidence: FieldConfidences,
    /// Raw OCR detection boxes this parse was built from (padded-image pixel
    /// coordinates). Populated on a real scan; empty on reformat. Intended for
    /// debugging / E2E snapshot-vs-live geometry diffing, not the normal UI.
    pub detections: Vec<OcrDetection>,
}

/// One OCR detection box as emitted by the engine, surfaced on
/// [`ReceiptResult`] for debugging. Mirrors [`DetectionInput`]'s shape.
#[derive(uniffi::Record)]
pub struct OcrDetection {
    /// Polygon points as `[x0, y0, x1, y1, …]` in padded-image pixels.
    pub points_xy: Vec<f64>,
    pub text: String,
    pub confidence: f64,
}

/// One OCR detection box for [`parse_detections`] (no ONNX required).
#[derive(uniffi::Record)]
pub struct DetectionInput {
    /// Quad or polygon points as `[x0, y0, x1, y1, …]` (at least 4 points).
    pub points_xy: Vec<f64>,
    pub text: String,
    pub confidence: f64,
}

/// Optional parse/scan knobs (rule overlays). Empty list = bundled defaults.
#[derive(uniffi::Record, Clone, Debug, Default)]
pub struct ParseOptions {
    /// Extra rule documents, later layers winning. Each is TOML that may carry
    /// any mix of `[[tags]]`, `[accounts]` and `[[rules]]` — so one document can
    /// declare a tag, map it to an account, and use it from a rule.
    ///
    /// Malformed TOML, an undeclared tag path, or a `disables` naming an unknown
    /// rule id all surface as [`ScanError::Parse`] rather than a panic.
    pub rule_documents: Vec<String>,
    /// Optional known-merchant keyword list; empty means bundled defaults.
    pub known_merchants: Vec<String>,
}

/// User edits applied by [`reformat_receipt`] without re-running OCR.
#[derive(uniffi::Record)]
pub struct ReceiptEdits {
    pub merchant: Option<String>,
    /// ISO `YYYY-MM-DD`.
    pub date_iso: Option<String>,
    /// Parallel to items; empty string means “no override for this index”.
    pub item_account_overrides: Vec<String>,
}

/// One item-classifier rule as it is actually in force — priorities already
/// boosted by layer, account already resolved.
#[derive(uniffi::Record, Clone, Debug)]
pub struct ItemRule {
    /// Provenance label (`legacy_0000`). Frozen and additive, so it is safe for
    /// a user document to name in `disables`.
    pub id: Option<String>,
    /// Position in the rule list — what [`RuleMatchInfo::rule_index`] refers to.
    pub index: u32,
    pub keywords: Vec<String>,
    /// Declared tag paths.
    pub tag_paths: Vec<String>,
    /// Tag paths this rule subtracts when it matches.
    pub remove_tags: Vec<String>,
    /// Rule ids this rule voids when it matches.
    pub disables: Vec<String>,
    /// The path that claims an account, or `None` for a tag-only rule.
    pub category_path: Option<String>,
    /// The account `category_path` resolves to.
    pub account: Option<String>,
    pub priority: i32,
    pub exact_only: bool,
    /// 0 = bundled defaults; 1+ = override documents, in the order supplied.
    pub layer: u32,
}

/// A tag-path -> beancount-account pair.
#[derive(uniffi::Record, Clone, Debug)]
pub struct ItemCategory {
    pub path: String,
    pub account: String,
}

/// One rule that fired for a description, and how strongly.
#[derive(uniffi::Record, Clone, Debug)]
pub struct RuleMatchInfo {
    pub rule_id: Option<String>,
    pub rule_index: u32,
    /// The keyword that actually hit — the specific reason this rule matched.
    pub matched_keyword: String,
    /// False when the hit came from the fuzzy/bigram stage rather than a literal
    /// or OCR-confusable substring match.
    pub is_exact: bool,
    pub priority: i32,
    pub keyword_length: u32,
    pub tag_paths: Vec<String>,
    pub category_path: Option<String>,
    /// True for the single match whose category won the ranking contest.
    pub is_category_winner: bool,
}

/// Why a description classifies the way it does.
#[derive(uniffi::Record, Clone, Debug)]
pub struct ItemExplanation {
    pub description: String,
    pub category_path: Option<String>,
    pub account: Option<String>,
    /// The tags the parser would put on this item.
    pub tags: Vec<ItemTag>,
    /// Every rule that fired **and survived subtraction**, strongest first.
    pub matches: Vec<RuleMatchInfo>,
}

/// Read access to the rule corpus in force: the bundled defaults plus any
/// override documents layered on top.
///
/// Construct once and reuse — it parses its documents up front, so `explain` is
/// cheap enough to call per keystroke.
#[derive(uniffi::Object)]
pub struct RuleBook {
    inner: CoreRuleBook,
}

#[uniffi::export]
impl RuleBook {
    /// Build from `options`. Returns [`ScanError::Parse`] on malformed TOML, an
    /// undeclared tag path, or a `disables` naming an unknown rule id — the same
    /// validation a scan applies, so this doubles as "would this document load?"
    #[uniffi::constructor]
    pub fn new(options: ParseOptions) -> Result<std::sync::Arc<Self>, ScanError> {
        let refs: Vec<&str> = options.rule_documents.iter().map(String::as_str).collect();
        let inner = CoreRuleBook::with_overrides(&refs).map_err(|msg| ScanError::Parse { msg })?;
        Ok(std::sync::Arc::new(RuleBook { inner }))
    }

    /// The declared tag vocabulary, in file order. Paths whose parent is also
    /// present form the tree.
    pub fn tags(&self) -> Vec<ItemTag> {
        self.inner
            .tag_vocabulary()
            .iter()
            .map(|node| ItemTag {
                path: node.path.clone(),
                display: node.display.clone(),
            })
            .collect()
    }

    /// Every tag path that maps to an account, sorted by path.
    pub fn categories(&self) -> Vec<ItemCategory> {
        self.inner
            .categories()
            .into_iter()
            .map(|c| ItemCategory {
                path: c.key,
                account: c.account,
            })
            .collect()
    }

    /// Every rule in force, in layer order.
    pub fn rules(&self) -> Vec<ItemRule> {
        self.inner
            .item_rules()
            .into_iter()
            .map(|r| ItemRule {
                id: r.id,
                index: r.index as u32,
                keywords: r.keywords,
                tag_paths: r.tags,
                remove_tags: r.remove_tags,
                disables: r.disables,
                category_path: r.category_key,
                account: r.account,
                priority: r.priority,
                exact_only: r.exact_only,
                layer: r.layer as u32,
            })
            .collect()
    }

    /// Why `description` classifies the way it does — the resolved account and
    /// tags, plus every rule that fired, strongest first.
    pub fn explain(&self, description: String) -> ItemExplanation {
        let e = self.inner.explain(&description);
        let label = |path: &String| ItemTag {
            display: self.inner.tag_display(path),
            path: path.clone(),
        };
        ItemExplanation {
            description: e.description,
            category_path: e.category_key,
            account: e.account,
            tags: e.tags.iter().map(label).collect(),
            matches: e
                .matches
                .into_iter()
                .map(|m| RuleMatchInfo {
                    rule_id: m.rule_id,
                    rule_index: m.rule_index as u32,
                    matched_keyword: m.matched_keyword,
                    is_exact: m.is_exact,
                    priority: m.priority,
                    keyword_length: m.keyword_length as u32,
                    tag_paths: m.tags,
                    category_path: m.category_key,
                    is_category_winner: m.is_category_winner,
                })
                .collect(),
        }
    }
}

/// Errors surfaced to Swift as a typed exception.
#[derive(Debug, uniffi::Error)]
pub enum ScanError {
    ModelLoad { msg: String },
    ImageDecode { msg: String },
    Inference { msg: String },
    Parse { msg: String },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::ModelLoad { msg } => write!(f, "failed to load OCR models: {msg}"),
            ScanError::ImageDecode { msg } => write!(f, "failed to decode image: {msg}"),
            ScanError::Inference { msg } => write!(f, "OCR/parse failed: {msg}"),
            ScanError::Parse { msg } => write!(f, "parse failed: {msg}"),
        }
    }
}

impl std::error::Error for ScanError {}

/// A loaded OCR pipeline. Construct once, reuse across scans. The engine needs
/// `&mut` for inference, so it's wrapped in a `Mutex` (UniFFI objects are shared
/// `Arc`s); scans on one session are therefore serialized.
#[derive(uniffi::Object)]
pub struct OcrSession {
    engine: Mutex<OcrEngine>,
}

#[uniffi::export]
impl OcrSession {
    /// Load the PP-OCRv5 models from `model_dir` (the bundle directory holding
    /// `PP-OCRv5_mobile_det.onnx`, `_rec.onnx`, `PP-LCNet…_ori.onnx`).
    ///
    /// When `use_orientation_cls` is false the textline-orientation classifier is
    /// not loaded or run: it fixes 180°-flipped lines, but captures from an
    /// upright document scan rarely need it, and skipping it removes the per-crop
    /// classify pass (~23% of on-device scan time). The caller reloads the
    /// session to change this.
    #[uniffi::constructor]
    pub fn new(model_dir: String, use_orientation_cls: bool) -> Result<Arc<Self>, ScanError> {
        let dir = std::path::Path::new(&model_dir);
        let cls_model = use_orientation_cls.then(|| dir.join(CLS_MODEL));
        let engine = OcrEngine::from_paths(dir.join(DET_MODEL), dir.join(REC_MODEL), cls_model)
            .map_err(|e| ScanError::ModelLoad { msg: e.to_string() })?;
        Ok(Arc::new(Self {
            engine: Mutex::new(engine),
        }))
    }

    /// Run the full image → beancount pipeline on encoded image bytes.
    pub fn scan(
        &self,
        image_bytes: Vec<u8>,
        today: DateYmd,
        credit_card_account: String,
        currency: String,
        tax_account: String,
    ) -> Result<ReceiptResult, ScanError> {
        self.scan_with_options(
            image_bytes,
            today,
            credit_card_account,
            currency,
            tax_account,
            ParseOptions {
                rule_documents: vec![],
                known_merchants: vec![],
            },
        )
    }

    /// Like [`Self::scan`] but applies optional rule overlays after OCR.
    ///
    /// The image still goes through the full ONNX path; classifier/merchant
    /// overrides affect only the parse/format half (so users can ship private
    /// rules without rebuilding the model bundle).
    pub fn scan_with_options(
        &self,
        image_bytes: Vec<u8>,
        today: DateYmd,
        credit_card_account: String,
        currency: String,
        tax_account: String,
        options: ParseOptions,
    ) -> Result<ReceiptResult, ScanError> {
        use std::time::Instant;

        let t_decode = Instant::now();
        let img = image::load_from_memory(&image_bytes)
            .map_err(|e| ScanError::ImageDecode { msg: e.to_string() })?
            .to_rgb8();
        let decode_ms = t_decode.elapsed().as_secs_f64() * 1e3;

        let image_sha256 = sha256_hex(&image_bytes);

        let mut engine = self.engine.lock().map_err(|e| ScanError::Inference {
            msg: format!("engine lock poisoned: {e}"),
        })?;

        // OCR with bundled defaults, then re-parse with overlays when requested.
        // process_image_timed always uses default rules; when overlays are set we
        // re-run parse from the same image by calling process_image_timed then
        // discarding parse… Actually process_image_timed embeds process_receipt
        // with defaults. For overlays, re-OCR is wasteful; we re-process by
        // using process_image_timed for timings+OCR and, when options non-empty,
        // we need detections. Simplest correct path: always use process_image_timed
        // when options empty; when options non-empty, recognize then process_with_options.
        let opts = to_process_options(&options);
        let has_overlay =
            !opts.item_classifier_override_tomls.is_empty() || opts.known_merchants.is_some();

        if !has_overlay {
            let (processed, timings) = process_image_timed(
                &mut engine,
                &img,
                "receipt.jpg",
                (today.year, today.month, today.day),
                &credit_card_account,
                &currency,
                &tax_account,
                Some(&image_sha256),
            )
            .map_err(|e| ScanError::Inference { msg: e.to_string() })?;
            return Ok(to_result(
                processed,
                ScanTimings::from_core(timings, decode_ms),
            ));
        }

        // Overlay path: reuse process_image_timed's prep/OCR by calling it, then
        // re-format is insufficient for category overlays — need re-parse.
        // Call timed path for stage timings, then re-parse detections via a second
        // process is hard without exposing detections. Fall back to full
        // process_image_timed (default rules) only for timings of OCR stages, and
        // separately re-run OCR… That's double. Better: use engine.recognize after prep.
        use receipt_core::ocr_transform::RawDetection as CoreRaw;
        use scan::{prepare_image as resize_and_pad, OCR_IMAGE_PADDING};

        let t = Instant::now();
        let prepared = resize_and_pad(&img);
        let prep_ms = t.elapsed().as_secs_f64() * 1e3;

        let (detections, ocr) = engine
            .recognize_image_timed(&prepared)
            .map_err(|e| ScanError::Inference { msg: e.to_string() })?;

        let raw: Vec<CoreRaw> = detections
            .into_iter()
            .map(|d| CoreRaw {
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
        let processed = process_receipt_with_options(
            raw,
            prepared.width() as i64,
            prepared.height() as i64,
            OCR_IMAGE_PADDING as i64,
            "receipt.jpg",
            (today.year, today.month, today.day),
            &credit_card_account,
            &currency,
            &tax_account,
            Some(&image_sha256),
            &opts,
        )
        .map_err(|msg| ScanError::Parse { msg })?;
        let parse_ms = t.elapsed().as_secs_f64() * 1e3;

        let timings = ScanTimings {
            spans: vec![
                PhaseSpan {
                    phase: Phase::Decode,
                    ms: decode_ms,
                },
                PhaseSpan {
                    phase: Phase::Prep,
                    ms: prep_ms,
                },
                PhaseSpan {
                    phase: Phase::Detect,
                    ms: ocr.detect_ms,
                },
                PhaseSpan {
                    phase: Phase::Classify,
                    ms: ocr.classify_ms,
                },
                PhaseSpan {
                    phase: Phase::Recognize,
                    ms: ocr.recognize_ms,
                },
                PhaseSpan {
                    phase: Phase::Parse,
                    ms: parse_ms,
                },
            ],
        };
        Ok(to_result(processed, timings))
    }
}

/// Parse OCR detections into a receipt **without** loading ONNX models.
///
/// Use after an external OCR backend, or to re-parse a frozen detection list
/// with different rule overlays. `points_xy` is a flat `[x,y,…]` list per box.
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn parse_detections(
    detections: Vec<DetectionInput>,
    padded_width: i64,
    padded_height: i64,
    padding: i64,
    image_filename: String,
    today: DateYmd,
    credit_card_account: String,
    currency: String,
    tax_account: String,
    image_sha256: Option<String>,
    options: ParseOptions,
) -> Result<ReceiptResult, ScanError> {
    let raw = detections_to_raw(detections)?;
    let opts = to_process_options(&options);
    let processed = process_receipt_with_options(
        raw,
        padded_width,
        padded_height,
        padding,
        &image_filename,
        (today.year, today.month, today.day),
        &credit_card_account,
        &currency,
        &tax_account,
        image_sha256.as_deref(),
        &opts,
    )
    .map_err(|msg| ScanError::Parse { msg })?;
    Ok(to_result(processed, ScanTimings::default()))
}

/// Re-render beancount after the user edits merchant / date / item accounts.
///
/// Does not re-run OCR. Pass the fields from a previous [`ReceiptResult`] plus
/// [`ReceiptEdits`]. Item account overrides are positional: index `i` applies
/// to `items[i]` when non-empty.
///
/// Round-trip `raw_text`, `image_filename`, and `tenders` from the prior scan so
/// multi-tender postings and card-last4 metadata survive the edit.
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn reformat_receipt(
    previous: ReceiptResult,
    today: DateYmd,
    credit_card_account: String,
    currency: String,
    tax_account: String,
    image_sha256: Option<String>,
    edits: ReceiptEdits,
    options: ParseOptions,
) -> Result<ReceiptResult, ScanError> {
    let parsed = receipt_result_to_parsed(&previous);
    let corrections = ReceiptCorrections {
        merchant: edits.merchant,
        date_iso: edits.date_iso,
        item_accounts: edits
            .item_account_overrides
            .into_iter()
            .map(|s| if s.is_empty() { None } else { Some(s) })
            .collect(),
    };
    let opts = to_process_options(&options);
    let processed = reformat_parsed_receipt(
        &parsed,
        (today.year, today.month, today.day),
        &credit_card_account,
        &currency,
        &tax_account,
        image_sha256.as_deref(),
        &corrections,
        Some(&opts),
    )
    .map_err(|msg| ScanError::Parse { msg })?;
    // Preserve prior timings (reformat is pure CPU, negligible).
    // Confidence is computed on the corrected parse inside reformat_parsed_receipt.
    Ok(to_result(processed, previous.timings))
}

fn detections_to_raw(detections: Vec<DetectionInput>) -> Result<Vec<RawDetection>, ScanError> {
    let mut raw = Vec::with_capacity(detections.len());
    for (i, d) in detections.into_iter().enumerate() {
        if d.points_xy.len() < 8 || d.points_xy.len() % 2 != 0 {
            return Err(ScanError::Parse {
                msg: format!(
                    "detection {i}: points_xy needs even length >= 8 (got {})",
                    d.points_xy.len()
                ),
            });
        }
        let points = d.points_xy.chunks_exact(2).map(|c| (c[0], c[1])).collect();
        raw.push(RawDetection {
            points,
            text: d.text,
            confidence: d.confidence,
        });
    }
    Ok(raw)
}

fn to_process_options(options: &ParseOptions) -> ProcessOptions {
    ProcessOptions {
        known_merchants: if options.known_merchants.is_empty() {
            None
        } else {
            Some(options.known_merchants.clone())
        },
        // `None` means "use the bundled families". This was hardcoded when the
        // field had no FFI counterpart, which silently made merchant-family
        // overrides unreachable from Swift and Kotlin; it stays `None` only
        // because nothing overrides them yet, and is now the single place to
        // change when something does.
        merchant_families: None,
        item_classifier_override_tomls: options.rule_documents.clone(),
    }
}

/// Flatten the rich `ProcessedReceipt` into the FFI record.
fn to_result(p: ProcessedReceipt, timings: ScanTimings) -> ReceiptResult {
    let vocabulary = p.tag_vocabulary.clone();
    let confidence = p.confidence.clone().into();
    let detections = p
        .detections
        .iter()
        .map(|det| OcrDetection {
            points_xy: det.points.iter().flat_map(|(x, y)| [*x, *y]).collect(),
            text: det.text.clone(),
            confidence: det.confidence,
        })
        .collect();
    let d = p.parsed;
    let warnings: Vec<ReceiptWarning> = d.warnings.iter().map(warning_to_ffi).collect();
    ReceiptResult {
        merchant: d.merchant,
        merchant_match: d.merchant_match.into(),
        date: d.date.map(|(y, m, day)| format!("{y:04}-{m:02}-{day:02}")),
        date_is_placeholder: d.date_is_placeholder,
        total: d.total,
        tax: d.tax,
        subtotal: d.subtotal,
        items: d
            .items
            .into_iter()
            .map(|i| ReceiptItem {
                description: i.description,
                price: i.price,
                quantity: i.quantity,
                account: i.account,
                tags: i
                    .tags
                    .iter()
                    .map(|path| ItemTag {
                        display: vocabulary
                            .iter()
                            .find(|node| &node.path == path)
                            .map(|node| node.display.clone())
                            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path).to_string()),
                        path: path.clone(),
                    })
                    .collect(),
            })
            .collect(),
        warnings,
        raw_text: d.raw_text,
        image_filename: d.image_filename,
        tenders: d
            .tenders
            .into_iter()
            .map(|t| ReceiptTender {
                amount: t.amount,
                account: t.account,
                kind: t.kind,
                raw_label: t.raw_label,
            })
            .collect(),
        beancount: p.beancount,
        beanbeaver_id: p.beanbeaver_id,
        document_relpath: p.document_relpath,
        timings,
        confidence,
        detections,
    }
}

/// The two warning-kind enums are the same vocabulary on either side of the
/// FFI, spelled twice because uniffi cannot re-export a foreign type. Keeping
/// them as exhaustive `match`es (never a catch-all) is what makes the compiler
/// point here the moment core grows a variant the clients would silently drop.
fn warning_kind_to_ffi(kind: CoreWarningKind) -> ReceiptWarningKind {
    match kind {
        CoreWarningKind::TotalMismatch => ReceiptWarningKind::TotalMismatch,
        CoreWarningKind::SubtotalMismatch => ReceiptWarningKind::SubtotalMismatch,
        CoreWarningKind::PossibleMissedItem => ReceiptWarningKind::PossibleMissedItem,
        CoreWarningKind::PriceAutoCorrected => ReceiptWarningKind::PriceAutoCorrected,
        CoreWarningKind::DroppedImplausiblePrice => ReceiptWarningKind::DroppedImplausiblePrice,
        CoreWarningKind::UncategorizedItem => ReceiptWarningKind::UncategorizedItem,
        CoreWarningKind::TenderMismatch => ReceiptWarningKind::TenderMismatch,
    }
}

fn warning_kind_to_core(kind: ReceiptWarningKind) -> CoreWarningKind {
    match kind {
        ReceiptWarningKind::TotalMismatch => CoreWarningKind::TotalMismatch,
        ReceiptWarningKind::SubtotalMismatch => CoreWarningKind::SubtotalMismatch,
        ReceiptWarningKind::PossibleMissedItem => CoreWarningKind::PossibleMissedItem,
        ReceiptWarningKind::PriceAutoCorrected => CoreWarningKind::PriceAutoCorrected,
        ReceiptWarningKind::DroppedImplausiblePrice => CoreWarningKind::DroppedImplausiblePrice,
        ReceiptWarningKind::UncategorizedItem => CoreWarningKind::UncategorizedItem,
        ReceiptWarningKind::TenderMismatch => CoreWarningKind::TenderMismatch,
    }
}

fn warning_to_ffi(w: &ParsedReceiptWarning) -> ReceiptWarning {
    ReceiptWarning {
        kind: warning_kind_to_ffi(w.kind),
        message: w.message.clone(),
        after_item_index: w.after_item_index.map(|i| i as i32).unwrap_or(-1),
    }
}

/// Rebuild `ParsedReceiptData` from a prior FFI result for reformat.
///
/// Carries tenders, raw_text, image_filename, and warning placement so
/// reformat does not drop multi-tender postings or card-last4 metadata.
fn receipt_result_to_parsed(r: &ReceiptResult) -> ParsedReceiptData {
    use receipt_core::merchant_match::MerchantMatch as CoreMM;
    let status = match r.merchant_match.status {
        MerchantMatchStatus::Exact => CoreStatus::Exact,
        MerchantMatchStatus::Corrected => CoreStatus::Corrected,
        MerchantMatchStatus::Suggested => CoreStatus::Suggested,
        MerchantMatchStatus::Unknown => CoreStatus::Unknown,
    };
    let date = r.date.as_ref().and_then(|iso| {
        let mut parts = iso.split('-');
        let y: i32 = parts.next()?.parse().ok()?;
        let m: u32 = parts.next()?.parse().ok()?;
        let d: u32 = parts.next()?.parse().ok()?;
        Some((y, m, d))
    });
    let image_filename = if r.image_filename.is_empty() {
        "receipt.jpg".into()
    } else {
        r.image_filename.clone()
    };
    ParsedReceiptData {
        merchant: r.merchant.clone(),
        merchant_match: CoreMM {
            raw: r.merchant_match.raw.clone(),
            canonical: r.merchant_match.canonical.clone(),
            status,
            score: r.merchant_match.score,
        },
        date,
        date_is_placeholder: r.date_is_placeholder,
        total: r.total.clone(),
        items: r
            .items
            .iter()
            .map(|i| ParsedReceiptItem {
                description: i.description.clone(),
                price: i.price.clone(),
                quantity: i.quantity,
                // The round-trip keeps the most specific tag path as the
                // category: it is the one that claimed the account, and the
                // shallower entries are its ancestors.
                category: i.tags.last().map(|t| t.path.clone()),
                account: i.account.clone(),
                tags: i.tags.iter().map(|t| t.path.clone()).collect(),
            })
            .collect(),
        tax: r.tax.clone(),
        subtotal: r.subtotal.clone(),
        raw_text: r.raw_text.clone(),
        image_filename,
        warnings: r
            .warnings
            .iter()
            .map(|w| {
                let after = if w.after_item_index >= 0 {
                    Some(w.after_item_index as usize)
                } else {
                    None
                };
                ParsedReceiptWarning {
                    kind: warning_kind_to_core(w.kind),
                    message: w.message.clone(),
                    after_item_index: after,
                }
            })
            .collect(),
        tenders: r
            .tenders
            .iter()
            .map(|t| ParsedReceiptTender {
                amount: t.amount.clone(),
                account: t.account.clone(),
                kind: t.kind.clone(),
                raw_label: t.raw_label.clone(),
            })
            .collect(),
    }
}

/// Lowercase hex SHA-256 of `bytes`, the receipt's content identity.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_previous() -> ReceiptResult {
        ReceiptResult {
            merchant: "COSTCO".into(),
            merchant_match: MerchantMatch {
                raw: "COSTCO".into(),
                canonical: Some("COSTCO".into()),
                status: MerchantMatchStatus::Exact,
                score: 1.0,
            },
            date: Some("2026-02-18".into()),
            date_is_placeholder: false,
            total: "10.00".into(),
            tax: None,
            subtotal: Some("10.00".into()),
            items: vec![ReceiptItem {
                description: "Milk".into(),
                price: "10.00".into(),
                quantity: 1,
                account: Some("Expenses:Food:Grocery:Dairy".into()),
                tags: vec![],
            }],
            warnings: vec![],
            raw_text: "COSTCO\n**** 1234\nTOTAL 10.00".into(),
            image_filename: "costco.jpg".into(),
            tenders: vec![ReceiptTender {
                amount: "10.00".into(),
                account: None,
                kind: "card".into(),
                raw_label: "MASTERCARD".into(),
            }],
            beancount: String::new(),
            beanbeaver_id: None,
            document_relpath: None,
            timings: ScanTimings::default(),
            confidence: FieldConfidences {
                merchant: 1.0,
                date: 1.0,
                total: 1.0,
                items_categorized: 1.0,
                needs_review: false,
            },
            detections: vec![],
        }
    }

    #[test]
    fn parse_detections_rejects_short_points() {
        let result = parse_detections(
            vec![DetectionInput {
                points_xy: vec![0.0, 0.0, 1.0, 1.0],
                text: "x".into(),
                confidence: 0.9,
            }],
            100,
            100,
            0,
            "t.jpg".into(),
            DateYmd {
                year: 2026,
                month: 1,
                day: 1,
            },
            "Liabilities:CreditCard".into(),
            "CAD".into(),
            "Expenses:Tax:HST".into(),
            None,
            ParseOptions {
                rule_documents: vec![],
                known_merchants: vec![],
            },
        );
        assert!(matches!(result, Err(ScanError::Parse { .. })));
    }

    #[test]
    fn reformat_receipt_changes_merchant_in_beancount() {
        let edited = reformat_receipt(
            sample_previous(),
            DateYmd {
                year: 2026,
                month: 2,
                day: 18,
            },
            "Liabilities:CreditCard".into(),
            "CAD".into(),
            "Expenses:Tax:HST".into(),
            Some("deadbeef".into()),
            ReceiptEdits {
                merchant: Some("Costco Wholesale".into()),
                date_iso: Some("2026-02-19".into()),
                item_account_overrides: vec!["Expenses:Food:Grocery:Dairy".into()],
            },
            ParseOptions {
                rule_documents: vec![],
                known_merchants: vec![],
            },
        )
        .expect("reformat");
        assert_eq!(edited.merchant, "Costco Wholesale");
        assert_eq!(edited.date.as_deref(), Some("2026-02-19"));
        assert!(edited.beancount.contains("Costco Wholesale"));
        assert!(edited.beancount.contains("2026-02-19"));
        // Round-trip OCR metadata / tenders.
        assert_eq!(edited.raw_text, "COSTCO\n**** 1234\nTOTAL 10.00");
        assert_eq!(edited.image_filename, "costco.jpg");
        assert_eq!(edited.tenders.len(), 1);
        assert!(!edited.confidence.needs_review);
        // Classifier key preserved despite account override in beancount.
        assert_eq!(
            edited.items[0].account.as_deref(),
            Some("Expenses:Food:Grocery:Dairy")
        );
    }

    #[test]
    fn reformat_receipt_rejects_bad_date() {
        let err = reformat_receipt(
            sample_previous(),
            DateYmd {
                year: 2026,
                month: 2,
                day: 18,
            },
            "Liabilities:CreditCard".into(),
            "CAD".into(),
            "Expenses:Tax:HST".into(),
            None,
            ReceiptEdits {
                merchant: None,
                date_iso: Some("bogus".into()),
                item_account_overrides: vec![],
            },
            ParseOptions {
                rule_documents: vec![],
                known_merchants: vec![],
            },
        );
        assert!(
            matches!(err, Err(ScanError::Parse { .. })),
            "expected Parse error for bad date_iso"
        );
    }

    #[test]
    fn parse_detections_rejects_bad_override_toml() {
        let result = parse_detections(
            vec![DetectionInput {
                points_xy: vec![0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0],
                text: "TOTAL 1.00".into(),
                confidence: 0.9,
            }],
            100,
            100,
            0,
            "t.jpg".into(),
            DateYmd {
                year: 2026,
                month: 1,
                day: 1,
            },
            "Liabilities:CreditCard".into(),
            "CAD".into(),
            "Expenses:Tax:HST".into(),
            None,
            ParseOptions {
                rule_documents: vec!["not {{ valid toml".into()],
                known_merchants: vec![],
            },
        );
        assert!(matches!(result, Err(ScanError::Parse { .. })));
    }

    // Full FFI round-trip on the committed fixture. Mirrors ocr-paddle's
    // end-to-end test but exercises the Swift-facing entry points.
    //   cargo test -p bb-receipt-ffi -- --ignored --nocapture
    #[test]
    #[ignore = "needs converted models + fixture"]
    fn scan_costco_fixture_end_to_end() {
        let session = OcrSession::new("../../models".to_string(), true).expect("load models");
        let bytes = std::fs::read("../../tests/receipts_e2e/costco_20260218_redact.jpg")
            .expect("read fixture");

        let r = session
            .scan(
                bytes,
                DateYmd {
                    year: 2026,
                    month: 2,
                    day: 18,
                },
                "Liabilities:CreditCard".to_string(),
                "CAD".to_string(),
                "Expenses:Tax:HST".to_string(),
            )
            .expect("scan");

        assert_eq!(r.merchant, "COSTCO");
        assert_eq!(r.date.as_deref(), Some("2026-02-18"));
        assert_eq!(r.total, "221.97");
        assert_eq!(r.tax.as_deref(), Some("4.44"));
        assert!(!r.items.is_empty());
        assert!(r.beancount.contains("COSTCO"));
        assert!(!r.confidence.needs_review || r.confidence.merchant >= 0.7);
        println!("{}", r.beancount);

        let t = &r.timings;
        for s in &t.spans {
            println!("  timing {:?}: {:.1}ms", s.phase, s.ms);
        }
        let total: f64 = t.spans.iter().map(|s| s.ms).sum();
        assert!(total > 0.0, "sum of phase spans should be positive");
        assert!(
            t.spans.iter().any(|s| s.phase == Phase::Parse),
            "expected a Parse span in on-device timings"
        );
    }

    /// The rule corpus must be readable from the FFI boundary — this is what the
    /// iOS browser is built on, and none of it crossed before.
    #[test]
    fn rule_book_exposes_vocabulary_categories_and_rules() {
        let book = RuleBook::new(ParseOptions::default()).expect("bundled book loads");
        let tags = book.tags();
        assert!(tags
            .iter()
            .any(|t| t.path == "grocery/dairy" && t.display == "Dairy"));
        // Authored display, not a capitalized segment — that is what produced
        // "Energy_drink" on screen.
        assert!(tags
            .iter()
            .any(|t| t.path == "grocery/drink/energy_drink" && t.display == "Energy Drink"));
        assert!(book
            .categories()
            .iter()
            .any(|c| c.path == "grocery/dairy" && c.account == "Expenses:Food:Grocery:Dairy"));
        assert!(book
            .rules()
            .iter()
            .any(|r| r.id.as_deref() == Some("legacy_0000")));
    }

    /// `explain` is the query tool: it must name the winning rule and the exact
    /// keyword that fired.
    #[test]
    fn rule_book_explains_a_description() {
        let book = RuleBook::new(ParseOptions::default()).expect("bundled book loads");
        let explained = book.explain("KS ORG 2% MILK".to_string());
        assert_eq!(
            explained.account.as_deref(),
            Some("Expenses:Food:Grocery:Dairy")
        );
        assert!(explained
            .tags
            .iter()
            .any(|t| t.path == "grocery/dairy/milk"));
        let winner = explained
            .matches
            .iter()
            .find(|m| m.is_category_winner)
            .expect("some rule won the category");
        assert_eq!(winner.category_path.as_deref(), Some("grocery/dairy"));
        assert!(!winner.matched_keyword.is_empty());
    }

    /// Constructing a book is also the import validator: the three ways a user
    /// document can be wrong each surface as a typed error, not a panic.
    #[test]
    fn rule_book_rejects_bad_documents_with_typed_errors() {
        for (doc, needle) in [
            ("this is not { valid toml", "invalid"),
            (
                "[[rules]]\nid=\"x\"\nkeywords=[\"Z\"]\ntags=[\"grocery/diary\"]",
                "grocery/diary",
            ),
            (
                "[[rules]]\nid=\"x\"\nkeywords=[\"Z\"]\ntags=[\"grocery/staple\"]\ndisables=[\"nope_9999\"]",
                "nope_9999",
            ),
        ] {
            let err = RuleBook::new(ParseOptions {
                rule_documents: vec![doc.to_string()],
                known_merchants: vec![],
            })
            .err()
            .expect("bad document must not load");
            let msg = err.to_string();
            assert!(msg.contains(needle), "expected {needle:?} in {msg:?}");
        }
    }

    /// An item's tags arrive as labelled node paths, least specific first, so a
    /// consumer renders the tree without reconstructing it.
    #[test]
    fn parsed_items_carry_labelled_tag_paths() {
        let book = RuleBook::new(ParseOptions::default()).expect("bundled book loads");
        let explained = book.explain("ROTISSERIE CHICKEN".to_string());
        let paths: Vec<&str> = explained.tags.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(paths.first(), Some(&"grocery"), "least specific first");
        assert!(paths.contains(&"grocery/meat/chicken"));
        assert!(explained.tags.iter().all(|t| !t.display.is_empty()));
    }
}
