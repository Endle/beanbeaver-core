use std::collections::HashSet;

use crate::categories;
use crate::common::ReceiptWarningKind;
use crate::date::Date;
use crate::fields;
use crate::money::Money;
use crate::parse_helpers;
use crate::spatial;
use crate::text;

#[derive(Clone, Debug)]
pub struct ParserRuleLayers {
    pub category_rules: categories::CategoryRuleLayers,
    pub account_mapping: Vec<(String, String)>,
    /// Per-merchant abbreviation tables, applied before classification so that
    /// a chain's fixed-width shorthand (`KS LIQ LNDRY`) can reach keywords that
    /// are spelled out in full. See [`crate::merchant_vocab`].
    pub merchant_vocab: Vec<crate::merchant_vocab::MerchantVocab>,
}

#[derive(Clone, Debug)]
pub struct ParsedReceiptItem {
    pub description: String,
    pub price: Money,
    pub quantity: i32,
    /// The winning rule's declared tag path (`grocery/dairy`), or `None`.
    ///
    /// Named `tag_path` and not `category` because it held both: the scan path
    /// wrote a tag path here and [`item_with_tag_path`] wrote a beancount
    /// account, so the same field meant different things depending on whether a
    /// receipt had been scanned or corrected. Nothing could tell them apart —
    /// the E2E harness had to compare a tag path against an account by resolving
    /// both through the account map and hoping.
    pub tag_path: Option<String>,
    /// The beancount account `tag_path` resolves to, resolved at parse time so
    /// consumers do not have to carry the account map around. `None` when the
    /// path claims no account.
    pub account: Option<String>,
    /// The beanbeaver-internal semantic classification for this line — a
    /// multi-tag view (e.g. `["grocery", "meat", "chicken"]`) that is upstream
    /// of, and richer than, the single `account` this resolves to. Consumers
    /// (the app UI) can present or filter on tags without reverse-engineering
    /// the account path. Empty when no classifier rule matched.
    pub tags: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ParsedReceiptWarning {
    pub kind: ReceiptWarningKind,
    pub message: String,
    pub after_item_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ParsedReceiptTender {
    pub amount: Money,
    pub account: Option<String>,
    pub kind: String,
    pub raw_label: String,
}

#[derive(Clone, Debug)]
pub struct ParsedReceiptData {
    /// Display merchant name: the canonical family when confidently resolved,
    /// otherwise the raw OCR text. Equal to `merchant_match.display()`.
    pub merchant: String,
    /// Full merchant resolution (raw OCR text, canonical family, confidence),
    /// for consumers that want to surface the correction to the user.
    pub merchant_match: crate::merchant_match::MerchantMatch,
    /// Contact and branch details printed on the receipt. These are parsed
    /// evidence, not a geocoded or otherwise verified location.
    pub merchant_details: crate::merchant_details::MerchantDetails,
    pub date: Option<Date>,
    pub date_is_placeholder: bool,
    pub total: Money,
    pub items: Vec<ParsedReceiptItem>,
    pub tax: Option<Money>,
    pub subtotal: Option<Money>,
    pub raw_text: String,
    pub image_filename: String,
    pub warnings: Vec<ParsedReceiptWarning>,
    pub tenders: Vec<ParsedReceiptTender>,
}

/// Some merchants print a line-item discount with NO sign — e.g. FreshCo's
/// "INSTANT SAVINGS $5.00" — unlike Costco's trailing-minus ("4.00-") or the
/// leading-minus Asian-grocery form. Such a line is a reduction, so its price
/// must be negative. Detection is keyword-based on the resolved item
/// description. Summary lines ("TOTAL SAVINGS", "Your Total Savings") are
/// filtered upstream and never reach here as items; the `TOTAL` guard is a
/// belt-and-suspenders exclusion in case one slips through.
fn is_unsigned_discount_line(description: &str) -> bool {
    let upper = description.to_ascii_uppercase();
    if upper.contains("TOTAL") {
        return false;
    }
    upper.contains("SAVINGS")
}

fn resolve_account_target(
    target: Option<&str>,
    rule_layers: &ParserRuleLayers,
    default: Option<&str>,
) -> Option<String> {
    match target {
        None => default.map(str::to_string),
        Some(raw) => {
            let cleaned = raw.trim();
            if cleaned.is_empty() {
                return default.map(str::to_string);
            }
            if cleaned.starts_with("Expenses:") {
                return Some(cleaned.to_string());
            }
            for (key, mapped) in &rule_layers.account_mapping {
                if key == cleaned {
                    return Some(mapped.clone());
                }
            }
            default.map(str::to_string)
        }
    }
}

fn categorize_description(description: &str, rule_layers: &ParserRuleLayers) -> Option<String> {
    let category_key =
        categories::classify_item_key(description, &rule_layers.category_rules, None);
    resolve_account_target(category_key.as_deref(), rule_layers, None)
}

/// The internal semantic tags for an item description — the multi-tag layer that
/// sits upstream of the single beancount account `categorize_description`
/// resolves. Classified from the same source string so the two agree.
fn item_tags(description: &str, rule_layers: &ParserRuleLayers) -> Vec<String> {
    categories::classify_item_tags(description, &rule_layers.category_rules)
}

/// Assemble one parsed item, applying the merchant's abbreviation vocabulary.
///
/// `category_source` is the string the extractor decided should drive
/// classification — the same line as `description` on the spatial path, but a
/// separate (often longer) source on the text path. Both get expanded, so the
/// recovered name and the category always agree about what the item is.
///
/// When `vocab` is `None` — no merchant table, or a merchant that never
/// resolved — every expansion is a no-op and this is exactly the old behavior.
fn build_item(
    description: String,
    price: Money,
    quantity: i32,
    category_source: &str,
    rule_layers: &ParserRuleLayers,
    vocab: Option<&crate::merchant_vocab::MerchantVocab>,
) -> ParsedReceiptItem {
    // Expansion is a **fallback, never an override**. Classify the printed text
    // first and keep that answer whenever it produces one; only reach for the
    // expanded reading when the shorthand classified to nothing.
    //
    // This is load-bearing in two directions, both found by corpus measurement:
    //
    //  - Expansions can collide with another category. `CQLDWTR -> Cold Water`
    //    turns Tide detergent into "TIDE Cold Water", and `WATER` is a Drink
    //    keyword — so an override would have filed laundry soap as a beverage.
    //  - Several existing rules are keyed to the *abbreviated* text
    //    (`KS BAGS 60`, `TIDE CQLDWTR`, `KS ORG 2%`). Rewriting the string out
    //    from under them makes those rules stop matching.
    //
    // Fallback ordering sidesteps both: a line the rules already understand is
    // untouched, and expansion can only ever fill a gap.
    let printed_category = categorize_description(category_source, rule_layers);
    let (tag_path, tags) = if printed_category.is_some() {
        (printed_category, item_tags(category_source, rule_layers))
    } else {
        match vocab.and_then(|vocab| {
            crate::merchant_vocab::expand_for_classification(category_source, vocab)
        }) {
            Some(expanded) => (
                categorize_description(&expanded, rule_layers),
                item_tags(&expanded, rule_layers),
            ),
            None => (None, item_tags(category_source, rule_layers)),
        }
    };

    // The printed text stays the leading part of the description: it is what the
    // receipt actually says, so it must survive for ledger review and for
    // matching against a bank line. The recovered reading is appended, not
    // substituted.
    let description = match vocab
        .and_then(|v| crate::merchant_vocab::expand(&description, v))
        .and_then(|recovered| crate::merchant_vocab::recovered_tail(&description, &recovered))
    {
        Some(tail) => format!("{description} ({tail})"),
        None => description,
    };

    let account = categories::resolve_account_target(
        tag_path.as_deref(),
        &rule_layers.category_rules.account_mapping,
        None,
    );

    ParsedReceiptItem {
        description,
        price,
        quantity,
        tag_path,
        account,
        tags,
    }
}

/// Build one item the way a scan does: classify `description` with the rules in
/// force and take the account that classification resolves to.
///
/// Public for the reformat path. A renamed line has to be re-classified by the
/// same rules a scanned one is, or it would keep the tags of the text it
/// replaced — which is the whole failure the user was correcting.
///
/// No merchant-vocabulary expansion, unlike [`build_item`]: expansion recovers a
/// chain's fixed-width shorthand, and a description the user typed is not
/// shorthand.
pub fn classified_item(
    description: String,
    price: Money,
    quantity: i32,
    rule_layers: &ParserRuleLayers,
) -> ParsedReceiptItem {
    let tag_path = categorize_description(&description, rule_layers);
    let account = categories::resolve_account_target(
        tag_path.as_deref(),
        &rule_layers.category_rules.account_mapping,
        None,
    );
    ParsedReceiptItem {
        description: description.clone(),
        price,
        quantity,
        tag_path,
        account,
        tags: item_tags(&description, rule_layers),
    }
}

/// The account a **user-chosen** tag path posts to: the path's own mapping, else
/// the nearest mapped ancestor's.
///
/// The ancestor walk is deliberate and is scoped to this entry point. Rule
/// authors get exact lookup (see [`categories::resolve_account_target`]) because
/// they can declare precisely what they mean, and 11 of the 42 bundled tags are
/// intentionally account-less — `grocery/meat/chicken` and `grocery/dairy/milk`
/// among them, which carry display detail while a broader rule supplies the
/// account. A person picking "Chicken" in a category list has no such second
/// rule to lean on, and exact lookup would file their correction to
/// `Expenses:FIXME`.
///
/// Roots with no mapping at all (`grocery`, `household`, `health`, `shopping`,
/// `restaurant`, `alcohol`, `gift_card`) still resolve to nothing and fall back
/// to the default account. That is a gap in the bundled account map rather than
/// in this walk.
fn account_for_chosen_tag(tag_path: &str, rule_layers: &ParserRuleLayers) -> Option<String> {
    let mut path = Some(tag_path);
    while let Some(candidate) = path {
        if let Some(account) = categories::resolve_account_target(
            Some(candidate),
            &rule_layers.category_rules.account_mapping,
            None,
        ) {
            return Some(account);
        }
        path = categories::TagNode::parent(candidate);
    }
    None
}

/// Build one item carrying the tag the user picked, bypassing the classifier.
///
/// `tag_path` must be declared by the rule corpus in force. An unknown path is
/// an error for the same reason a rule document naming one is: silently
/// dropping it would hand back a line tagged with nothing, which reads as the
/// edit having been ignored.
pub fn item_with_tag_path(
    description: String,
    price: Money,
    quantity: i32,
    tag_path: &str,
    rule_layers: &ParserRuleLayers,
) -> Result<ParsedReceiptItem, String> {
    if !rule_layers
        .category_rules
        .tag_vocabulary
        .iter()
        .any(|node| node.path == tag_path)
    {
        return Err(format!(
            "unknown tag path \"{tag_path}\" — not declared by the rules in force"
        ));
    }
    let account = account_for_chosen_tag(tag_path, rule_layers);
    Ok(ParsedReceiptItem {
        description,
        price,
        quantity,
        // The path the user picked, not the account it resolves to. Storing the
        // account here is what made this field ambiguous: a corrected line
        // reported `Expenses:Food:Grocery:Dairy` where a scanned one reported
        // `grocery/dairy`, and `account` already carries the account anyway.
        tag_path: Some(tag_path.to_string()),
        account,
        tags: categories::expand_tag_paths(std::slice::from_ref(&tag_path.to_string())),
    })
}

/// The findings that follow from arithmetic alone — the item block against the
/// summary amounts — and so are the findings a user edit can invalidate.
///
/// Lifted out of [`parse_receipt`] verbatim so [`crate::process::reformat_parsed_receipt`]
/// can recompute them after the user rewrites the item list. That is what lets a
/// corrected receipt stop warning: left as parsed, a `TotalMismatch` would
/// outlive the mismatch it describes and the app would badge a receipt that now
/// balances.
///
/// Emission order is the order `parse_receipt` has always used, and callers rely
/// on it — the arithmetic findings come before the tender finding and before the
/// per-item ones.
pub fn balance_warnings(
    items: &[ParsedReceiptItem],
    total_cents: Money,
    tax_cents: Option<Money>,
    subtotal_cents: Option<Money>,
) -> Vec<ParsedReceiptWarning> {
    let mut warnings: Vec<ParsedReceiptWarning> = Vec::new();

    // Postings that overshoot the receipt total cannot balance, and until now
    // nothing said so. `formatter` closes an *undershoot* with an
    // `Expenses:FIXME` remainder — the ordinary "we missed an item" case, 26 of
    // 125 corpus receipts — but has no answer for the other direction and
    // silently emits a transaction beancount will reject. Overshoot is always a
    // defect: an item is duplicated, or a summary amount was parsed as an item.
    // It is also rare and specific — 6 of 125 receipts, every one genuinely
    // wrong — so warning on it is a signal, not noise.
    // Every balance check below is gated on `total_cents > 0`, which reads as a
    // guard and behaves as an off switch: a receipt whose total came back zero
    // doesn't fail those checks, it skips them. A C&C slip parsed 23 items and a
    // $0.00 total and reported nothing at all — the one shape where the entry is
    // certainly wrong was the one shape that stayed quiet.
    //
    // Items alone are enough to know a total was expected. A genuinely zero
    // receipt has nothing to post, so it never reaches here with items in hand.
    if total_cents == Money::ZERO && !items.is_empty() {
        warnings.push(ParsedReceiptWarning {
            kind: ReceiptWarningKind::ImplausibleSummary,
            message: format!(
                "no receipt total could be read, but {} item{} parsed totalling {} — the entry cannot be trusted to balance",
                items.len(),
                if items.len() == 1 { " was" } else { "s were" },
                items.iter().map(|item| item.price).sum::<Money>(),
            ),
            after_item_index: None,
        });
    }

    // Subtotal equal to tax is not a near-miss, it is arithmetically impossible:
    // it says the receipt taxed at 100%. What it really means is that one amount
    // was handed to both labels, which happens when the label column and the
    // amount column drift apart — on the C&C slip a single `153.55` overlapped
    // both `Sub Total` and `HST` while the true total sat unclaimed a row below.
    //
    // Nothing is repaired. Which label should have kept the amount is not
    // recoverable from the arithmetic, exactly as with `TenderMismatch`, and the
    // real fix is in grouping. Zero is excluded: a genuinely untaxed receipt
    // prints 0.00 for both, and that is a fact rather than a fault.
    if let (Some(subtotal), Some(tax)) = (subtotal_cents, tax_cents) {
        if subtotal == tax && subtotal != Money::ZERO {
            warnings.push(ParsedReceiptWarning {
                kind: ReceiptWarningKind::ImplausibleSummary,
                message: format!(
                    "subtotal and tax both read {} — they cannot both be right, so one of the summary amounts is misread",
                    subtotal,
                ),
                after_item_index: None,
            });
        }
    }

    if total_cents > Money::ZERO {
        let posted_cents =
            items.iter().map(|item| item.price).sum::<Money>() + tax_cents.unwrap_or(Money::ZERO);
        if posted_cents > total_cents {
            warnings.push(ParsedReceiptWarning {
                kind: ReceiptWarningKind::TotalMismatch,
                message: format!(
                    "items{} total {} but the receipt total is {} — {} too much, so this transaction will not balance",
                    if tax_cents.is_some() { " and tax" } else { "" },
                    posted_cents,
                    total_cents,
                    posted_cents - total_cents,
                ),
                after_item_index: None,
            });
        }

        // Undershoot is the other half, and it used to say nothing at all: the
        // formatter quietly closes the gap with an `Expenses:FIXME` remainder,
        // so a receipt could be *entirely* mis-paired and still report clean.
        // A No Frills scan lost one item and gave the remaining three their
        // neighbours' prices, and the only trace was a 3.78 plug nobody saw.
        //
        // Warning on every undershoot is noise — 23 of 122 receipts, most of
        // them a few cents. What makes it a signal is asking the question
        // against the printed SUBTOTAL instead of the total: fees, deposits,
        // rounding and tax defects all live *between* subtotal and total, so
        // measuring there is what mixes them into the item-block question.
        //
        // Measured over the corpus, the two deltas triangulate:
        //
        //   items != subtotal, posted != total -> the item block is wrong (16)
        //   items != subtotal, posted == total -> the SUBTOTAL was misread (2)
        //   items == subtotal, posted != total -> items fine, tax/fees (9)
        //
        // Only the first is a missing or spurious line, and requiring both to
        // disagree is what removes the entire sub-dollar noise band — every
        // 9c/10c/15c/20c case in the corpus lands in the third bucket.
        if let Some(subtotal_cents) = subtotal_cents {
            let items_cents = posted_cents - tax_cents.unwrap_or(Money::ZERO);
            let item_block_delta = items_cents - subtotal_cents;
            if item_block_delta != Money::ZERO && posted_cents != total_cents {
                let (verb, amount) = if item_block_delta < Money::ZERO {
                    ("short of", -item_block_delta)
                } else {
                    ("more than", item_block_delta)
                };
                warnings.push(ParsedReceiptWarning {
                    kind: ReceiptWarningKind::SubtotalMismatch,
                    message: format!(
                        "items total {}, {} the receipt's subtotal of {} by {} — a line was probably {}",
                        items_cents,
                        verb,
                        subtotal_cents,
                        amount,
                        if item_block_delta < Money::ZERO { "missed" } else { "counted twice" },
                    ),
                    after_item_index: None,
                });
            }
        }
    }

    warnings
}

/// "This line matched no classifier rule", per item. Also recomputed on the
/// reformat path: retagging a line is precisely the edit that clears it.
pub fn uncategorized_warnings(items: &[ParsedReceiptItem]) -> Vec<ParsedReceiptWarning> {
    let mut warnings: Vec<ParsedReceiptWarning> = Vec::new();

    // "This line matched no classifier rule" is a finding like any other, and
    // the parser is the only layer that knows it first-hand. It used to be
    // re-derived by each client from `tags.is_empty()`, which is how a
    // perfectly-parsed discount line ended up indistinguishable from a product
    // nobody has written a rule for. Reported last so the arithmetic warnings —
    // which are about the receipt as a whole — keep their existing position.
    for (index, item) in items.iter().enumerate() {
        if item.tags.is_empty() {
            warnings.push(ParsedReceiptWarning {
                kind: ReceiptWarningKind::UncategorizedItem,
                message: format!("no classifier rule matched \"{}\"", item.description),
                after_item_index: Some(index),
            });
        }
    }

    warnings
}

pub fn parse_receipt(
    doc: &crate::ocr_document::OcrDocument,
    rule_layers: &ParserRuleLayers,
    image_filename: &str,
    known_merchants: &[String],
    merchant_families: &[crate::merchant_match::MerchantFamily],
    current_year: i32,
) -> ParsedReceiptData {
    let document_text = doc.full_text();
    let full_text = document_text.as_str();
    let lines = full_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    // Rows standing in a print-grid column that never carries a price annotate
    // the item above them rather than being one (see
    // `spatial::annotation_line_flags`). The verdict is geometric, so
    // it has to be taken here: the text path below sees only strings, and the
    // chains this catches — Food Basics' `Saving 4.72` — word their annotations
    // in a vocabulary no keyword list has met yet.
    //
    // The flags index straight into `full_text.lines()`, which is what
    // [`OcrDocument`](crate::ocr_document::OcrDocument) guarantees: `full_text`
    // is its own lines joined with newlines and no line's text contains one, so
    // the two sequences are the same sequence. This used to be three parallel
    // parameters with a runtime `aligned` count-check guarding against callers
    // that built them independently; the document type makes that
    // unrepresentable. The `get` is not that check returning by another name —
    // it is bounds safety for the one way a hand-built document could still
    // break the 1:1 property, a line whose own text contains a newline.
    let annotation_flags = spatial::annotation_line_flags(doc);
    let item_lines: Vec<String> = full_text
        .lines()
        .enumerate()
        .filter(|(index, _)| !annotation_flags.get(*index).copied().unwrap_or(false))
        .map(|(_, line)| line.trim())
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();

    let merchant_match = parse_helpers::extract_merchant_match(
        &lines,
        full_text,
        doc,
        known_merchants,
        merchant_families,
    );
    let merchant = merchant_match.display().to_string();
    let merchant_details = crate::merchant_details::extract_merchant_details(&lines);
    // Scoped to the *canonical* family, not the raw OCR header: an unresolved
    // merchant gets no expansions, so the feature fails closed.
    let vocab = merchant_match.canonical.as_deref().and_then(|canonical| {
        crate::merchant_vocab::for_merchant(canonical, &rule_layers.merchant_vocab)
    });
    let parsed_date = fields::extract_date(&lines, full_text, current_year);
    let date = parsed_date;
    let date_is_placeholder = date.is_none();
    // `fields` still returns raw i64 cents; convert once, here, so
    // nothing below this line carries an untyped amount.
    let total_cents = Money::from_cents(fields::extract_total(&lines));
    let summary_reading = fields::extract_summary_reconciled(&lines, total_cents.cents());
    let tax_reading = &summary_reading.tax;
    let tax_cents = tax_reading.cents.map(Money::from_cents);
    let subtotal_cents = summary_reading.subtotal_cents.map(Money::from_cents);

    let mut summary_amounts = HashSet::new();
    if total_cents != Money::ZERO {
        summary_amounts.insert(total_cents);
    }
    if let Some(tax_cents) = tax_cents {
        summary_amounts.insert(tax_cents);
    }
    if let Some(subtotal_cents) = subtotal_cents {
        summary_amounts.insert(subtotal_cents);
    }

    let spatial_layout =
        doc.has_useful_bbox_data() && parse_helpers::is_spatial_layout_receipt(full_text);

    let (items, mut warnings): (Vec<ParsedReceiptItem>, Vec<ParsedReceiptWarning>) =
        if spatial_layout {
            let spatial_outcome = spatial::extract_spatial_items(doc);
            if spatial_outcome.items.is_empty() {
                let (items, warnings) = text::extract_text_items(&item_lines, &summary_amounts);
                (
                    items
                        .into_iter()
                        .map(|item| {
                            build_item(
                                item.description.clone(),
                                item.price,
                                item.quantity,
                                &item.category_source,
                                rule_layers,
                                vocab,
                            )
                        })
                        .collect(),
                    warnings
                        .into_iter()
                        .map(|warning| ParsedReceiptWarning {
                            kind: warning.kind,
                            message: warning.message,
                            after_item_index: warning.after_item_index,
                        })
                        .collect(),
                )
            } else {
                (
                    spatial_outcome
                        .items
                        .into_iter()
                        .map(|item| {
                            build_item(
                                item.description.clone(),
                                item.price,
                                1,
                                &item.description,
                                rule_layers,
                                vocab,
                            )
                        })
                        .collect(),
                    spatial_outcome
                        .warnings
                        .into_iter()
                        .map(|warning| ParsedReceiptWarning {
                            kind: warning.kind,
                            message: warning.message,
                            after_item_index: warning.after_item_index,
                        })
                        .collect(),
                )
            }
        } else {
            let (items, warnings) = text::extract_text_items(&item_lines, &summary_amounts);
            (
                items
                    .into_iter()
                    .map(|item| {
                        build_item(
                            item.description.clone(),
                            item.price,
                            item.quantity,
                            &item.category_source,
                            rule_layers,
                            vocab,
                        )
                    })
                    .collect(),
                warnings
                    .into_iter()
                    .map(|warning| ParsedReceiptWarning {
                        kind: warning.kind,
                        message: warning.message,
                        after_item_index: warning.after_item_index,
                    })
                    .collect(),
            )
        };

    // Sign-correct unsigned line-item discounts (e.g. FreshCo "INSTANT
    // SAVINGS $5.00"), covering both the spatial and text paths at their
    // single merge point.
    let items: Vec<ParsedReceiptItem> = items
        .into_iter()
        .map(|mut item| {
            if !item.price.is_negative() && is_unsigned_discount_line(&item.description) {
                item.price = -item.price;
            }
            item
        })
        .collect();

    // A tax the summary block could not state coherently was derived from
    // `SUBTOTAL + TAX = TOTAL` (see `fields::reconcile_tax`). Say so: the
    // derived amount is better than what was printed, but it is still an amount
    // this parser chose rather than read, and a silent rewrite of a money field
    // is exactly the kind of thing a reader should be able to check against the
    // photo.
    //
    // The whole-block repair gets its own wording rather than reusing the line
    // below. It rewrites the subtotal *and* the tax, and it does so because the
    // block's labels and amounts came apart — not because the identity implied a
    // better figure — so describing it as "the receipt's own subtotal implies"
    // would name as evidence the very field it just replaced.
    if summary_reading.shift_repaired() {
        if let (Some(printed), Some(subtotal), Some(tax)) = (
            summary_reading.printed_subtotal_cents,
            summary_reading.subtotal_cents,
            tax_reading.cents,
        ) {
            warnings.push(ParsedReceiptWarning {
                kind: ReceiptWarningKind::PriceAutoCorrected,
                message: format!(
                    "the summary block's labels and amounts are off by a row (subtotal read as {}) — re-read as subtotal {} and tax {}",
                    Money::from_cents(printed),
                    Money::from_cents(subtotal),
                    Money::from_cents(tax),
                ),
                after_item_index: None,
            });
        }
    } else if tax_reading.was_repaired() {
        if let (Some(printed), Some(cents)) = (tax_reading.printed_cents, tax_reading.cents) {
            warnings.push(ParsedReceiptWarning {
                kind: ReceiptWarningKind::PriceAutoCorrected,
                message: format!(
                    "tax read as {} but the receipt's own subtotal and total imply {} — using the implied amount",
                    Money::from_cents(printed),
                    Money::from_cents(cents),
                ),
                after_item_index: None,
            });
        }
    }

    warnings.extend(balance_warnings(
        &items,
        total_cents,
        tax_cents,
        subtotal_cents,
    ));

    // The payment block is an independent witness to the total: when a receipt
    // prints its tenders, they partition the total rather than echoing it, so
    // their sum is a second reading of the same number. Report the disagreement
    // — `extract_tenders` used to swallow it, returning nothing at all, which
    // made a misread amount look like a receipt with no payment block.
    //
    // Deliberately *only* a report. Which side is wrong is not recoverable from
    // the arithmetic (see `ReceiptWarningKind::TenderMismatch`), so the total
    // stands as parsed and `formatter` keeps the entry balanced by
    // falling back to a single payment posting.
    let tender_lines = fields::extract_tenders(&lines);
    if !fields::tenders_reconcile(&lines, &tender_lines, total_cents.cents()) {
        let net_cents = Money::from_cents(fields::tendered_net_cents(&lines, &tender_lines));
        warnings.push(ParsedReceiptWarning {
            kind: ReceiptWarningKind::TenderMismatch,
            message: format!(
                "payment lines account for {} but the receipt total is {} — {} unaccounted for, so one of the two is misread",
                net_cents,
                total_cents,
                (net_cents - total_cents).abs(),
            ),
            after_item_index: None,
        });
    }

    warnings.extend(uncategorized_warnings(&items));

    let tenders = tender_lines
        .into_iter()
        .map(|tender| ParsedReceiptTender {
            amount: Money::from_cents(tender.amount_cents),
            account: None,
            kind: tender.kind.to_string(),
            raw_label: tender.raw_label,
        })
        .collect();

    ParsedReceiptData {
        merchant,
        merchant_match,
        merchant_details,
        date,
        date_is_placeholder,
        total: total_cents,
        items,
        tax: tax_cents,
        subtotal: subtotal_cents,
        raw_text: full_text.to_string(),
        image_filename: image_filename.to_string(),
        warnings,
        tenders,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_unsigned_discount_line, item_tags};
    use crate::common::ReceiptWarningKind;
    use crate::money::Money;
    use crate::rules::default_parser_rule_layers;

    #[test]
    fn item_tags_are_the_multi_tag_classification() {
        let layers = default_parser_rule_layers();
        // A rotisserie chicken matches several rules — the meat rule
        // (grocery, meat), the semantic chicken tag, and the prepared-meal rule
        // — and their tags accumulate (deduped, first-seen order) onto one item.
        // Tags are node PATHS, least specific first, so the tree survives the
        // trip to a consumer without being reconstructed from bare segments.
        assert_eq!(
            item_tags("ROTISSERIE CHICKEN", &layers),
            vec![
                "grocery",
                "grocery/meat",
                "grocery/meat/chicken",
                "grocery/prepared_meal"
            ]
        );
        // Milk carries the dairy rule's tags plus its own semantic "milk" tag.
        assert_eq!(
            item_tags("MILK", &layers),
            vec!["grocery", "grocery/dairy", "grocery/dairy/milk"]
        );
        // An unrecognized line classifies to no tags rather than a guess.
        assert!(item_tags("ZZQW UNKNOWN ITEM", &layers).is_empty());
    }

    /// Parse plain text through the real pipeline: no bbox data, so the text
    /// path runs, which is all these balance assertions need.
    fn parse_text(text: &str) -> super::ParsedReceiptData {
        let layers = default_parser_rule_layers();
        super::parse_receipt(
            &crate::ocr_document::OcrDocument::from_text(text),
            &layers,
            "receipt.jpg",
            &[],
            &[],
            2026,
        )
    }

    /// Every finding of the kind, so a test can't pass on the wrong shape.
    fn implausible(parsed: &super::ParsedReceiptData) -> Vec<&super::ParsedReceiptWarning> {
        parsed
            .warnings
            .iter()
            .filter(|w| w.kind == ReceiptWarningKind::ImplausibleSummary)
            .collect()
    }

    #[test]
    fn warns_when_items_parsed_but_no_total_was_read() {
        // The C&C slip's shape: items parse, the total does not. Every balance
        // check is gated on `total > 0`, so this receipt used to satisfy all of
        // them by skipping all of them.
        let parsed = parse_text(
            "C&C SUPERMARKET\n\
             MILK 4.00\n\
             BREAD 3.50\n",
        );

        assert_eq!(
            parsed.total,
            Money::from_decimal_str("0.00"),
            "fixture must reach the zero-total path"
        );
        assert!(!parsed.items.is_empty(), "fixture must parse items");

        let found = implausible(&parsed);
        assert_eq!(found.len(), 1, "warnings were {:?}", parsed.warnings);
        assert!(
            found[0].message.contains("no receipt total"),
            "message should say the total is missing: {}",
            found[0].message
        );
    }

    #[test]
    fn does_not_warn_about_a_missing_total_when_nothing_was_parsed() {
        // A receipt that yielded no items has nothing to post, so there is no
        // entry to be wrong. Warning here would fire on every unreadable photo.
        let parsed = parse_text("SOME SHOP\nTHANK YOU FOR SHOPPING\n");

        assert!(parsed.items.is_empty(), "fixture must parse no items");
        assert!(
            implausible(&parsed).is_empty(),
            "warnings were {:?}",
            parsed.warnings
        );
    }

    #[test]
    fn warns_when_subtotal_equals_tax() {
        // One amount handed to two labels — arithmetically a 100% tax rate. On
        // the receipt that prompted this, a single `153.55` overlapped both
        // `Sub Total` and `HST` because the amount column had drifted a row.
        let parsed = parse_text(
            "C&C SUPERMARKET\n\
             MILK 4.00\n\
             BREAD 3.50\n\
             SUBTOTAL 7.50\n\
             HST 7.50\n\
             TOTAL 7.50\n",
        );

        let found = implausible(&parsed);
        assert_eq!(found.len(), 1, "warnings were {:?}", parsed.warnings);
        assert!(
            found[0].message.contains("7.50") && found[0].message.contains("subtotal and tax"),
            "message should name both labels and the shared amount: {}",
            found[0].message
        );
    }

    #[test]
    fn does_not_warn_when_an_untaxed_receipt_prints_zero_for_both() {
        // A genuinely untaxed receipt prints 0.00 for subtotal's tax line. Equal
        // is only impossible when the amount is nonzero.
        let parsed = parse_text(
            "SOME SHOP\n\
             MILK 4.00\n\
             SUBTOTAL 4.00\n\
             TAX 0.00\n\
             TOTAL 4.00\n",
        );

        assert!(
            implausible(&parsed).is_empty(),
            "warnings were {:?}",
            parsed.warnings
        );
    }

    #[test]
    fn reports_a_tax_derived_from_the_summary_identity() {
        // Foody Mart 2026-08-22: "HST 1.82" came back as "11:82", which carries
        // no parseable amount, so the tax read as the hst5% bucket's 0.00 and
        // 1.82 vanished from the entry without a word.
        let parsed = parse_text(
            "FOODY MART\n\
             Hot Food 13.99H\n\
             Sub Total 13.99\n\
             HST 11:82\n\
             hst5% 0.00\n\
             Total after Tax 15.81\n",
        );

        assert_eq!(parsed.tax, Some(Money::from_decimal_str("1.82")));
        let repaired: Vec<_> = parsed
            .warnings
            .iter()
            .filter(|w| w.kind == ReceiptWarningKind::PriceAutoCorrected)
            .collect();
        assert_eq!(repaired.len(), 1, "warnings were {:?}", parsed.warnings);
        assert!(
            repaired[0].message.contains("0.00") && repaired[0].message.contains("1.82"),
            "message should name both the printed and the derived tax: {}",
            repaired[0].message
        );
    }

    #[test]
    fn does_not_report_a_repair_when_the_tax_reads_cleanly() {
        let parsed = parse_text(
            "FOODY MART\n\
             Hot Food 13.99H\n\
             Sub Total 13.99\n\
             HST 1.82\n\
             hst5% 0.00\n\
             Total after Tax 15.81\n",
        );

        assert_eq!(parsed.tax, Some(Money::from_decimal_str("1.82")));
        assert!(
            !parsed
                .warnings
                .iter()
                .any(|w| w.kind == ReceiptWarningKind::PriceAutoCorrected),
            "warnings were {:?}",
            parsed.warnings
        );
    }

    #[test]
    fn warns_when_postings_overshoot_the_receipt_total() {
        // The No Frills scan that prompted this: line grouping gave the subtotal
        // its own item row, so the postings came to double the total and the
        // emitted transaction could never balance — silently, until now.
        let parsed = parse_text(
            "NOFRILLS\n\
             22-DAIRY MRJ 2.29\n\
             2% NATURAL YOGUR\n\
             27-PRODUCE WATERMLN SGRBABY MRJ 23.96\n\
             (4)4331 26.25\n\
             SUBTOTAL 26.25\n\
             TOTAL 26.25\n",
        );

        assert_eq!(parsed.total, Money::from_decimal_str("26.25"));
        let balance: Vec<_> = parsed
            .warnings
            .iter()
            .filter(|w| w.kind == ReceiptWarningKind::TotalMismatch)
            .collect();
        assert_eq!(balance.len(), 1, "warnings were {:?}", parsed.warnings);

        // Assert the message against the parse it describes rather than against
        // numbers copied from one scan: the text path reaches a different item
        // set than the spatial one, and a warning that misreports the amounts
        // would be worse than no warning at all.
        let posted: i64 = parsed.items.iter().map(|item| item.price.cents()).sum();
        assert!(posted > 2_625, "fixture should overshoot, posted {posted}");
        assert!(
            balance[0]
                .message
                .contains(&Money::from_cents(posted).to_string())
                && balance[0].message.contains("26.25")
                && balance[0]
                    .message
                    .contains(&Money::from_cents(posted - 2_625).to_string()),
            "message should name the posted total, the receipt total and the gap: {}",
            balance[0].message
        );
    }

    #[test]
    fn warns_when_items_disagree_with_the_printed_subtotal() {
        // The signal case: items fall short of the printed subtotal *and* the
        // postings miss the total, so a line was genuinely lost.
        let parsed = parse_text(
            "NOFRILLS\n\
             MILK 4.00\n\
             BREAD 3.00\n\
             SUBTOTAL 10.00\n\
             HST 1.30\n\
             TOTAL 11.30\n",
        );
        assert!(
            parsed
                .warnings
                .iter()
                .any(|w| w.kind == ReceiptWarningKind::SubtotalMismatch
                    && w.message.contains("short of the receipt's subtotal")),
            "warnings were {:?}",
            parsed.warnings
        );
    }

    #[test]
    fn does_not_warn_when_only_fees_sit_between_subtotal_and_total() {
        // items == subtotal exactly, but posted != total because of a deposit
        // between them. This is the whole sub-dollar noise band the corpus is
        // full of, and it must stay silent.
        let parsed = parse_text(
            "NOFRILLS\n\
             MILK 4.00\n\
             BREAD 3.00\n\
             SUBTOTAL 7.00\n\
             TOTAL 7.10\n",
        );
        assert!(
            !parsed
                .warnings
                .iter()
                .any(|w| w.kind == ReceiptWarningKind::SubtotalMismatch),
            "warnings were {:?}",
            parsed.warnings
        );
    }

    #[test]
    fn does_not_warn_when_postings_merely_undershoot() {
        // The ordinary "we missed an item" case — 26 of 125 corpus receipts.
        // `formatter` closes it with an `Expenses:FIXME` remainder, so
        // the transaction balances and there is nothing to report.
        let parsed = parse_text(
            "NOFRILLS\n\
             MILK 2.29\n\
             TOTAL 26.25\n",
        );

        assert_eq!(parsed.total, Money::from_decimal_str("26.25"));
        assert!(
            !parsed
                .warnings
                .iter()
                .any(|w| w.kind == ReceiptWarningKind::TotalMismatch),
            "warnings were {:?}",
            parsed.warnings
        );
    }

    #[test]
    fn flags_unsigned_savings_lines() {
        // FreshCo prints "INSTANT SAVINGS $5.00" with no minus sign.
        assert!(is_unsigned_discount_line("INSTANT SAVINGS $"));
        assert!(is_unsigned_discount_line("Member Savings"));
    }

    #[test]
    fn ignores_summary_and_regular_items() {
        // Summary rollups must never be sign-flipped as items.
        assert!(!is_unsigned_discount_line("TOTAL SAVINGS"));
        assert!(!is_unsigned_discount_line("Your Total Savings"));
        // Ordinary products are untouched.
        assert!(!is_unsigned_discount_line("Tom Diced"));
        assert!(!is_unsigned_discount_line("Soft Drink Orange"));
    }

    #[test]
    fn discount_lines_classify_as_discounts_not_as_nothing() {
        let layers = default_parser_rule_layers();
        // Costco: the discount code, then the code of the item it reduces.
        assert_eq!(item_tags("2087683 TPD/969786", &layers), vec!["discount"]);
        // FreshCo's two spellings.
        assert_eq!(item_tags("INSTANT SAVINGS", &layers), vec!["discount"]);
        assert_eq!(item_tags("YOU SAVED", &layers), vec!["discount"]);
        // A discount that names what it discounts nets against that category —
        // the rule sits *below* the product rules on purpose, so the product
        // wins the account while the line still picks up the `discount` tag.
        let both = item_tags("INSTANT SAVINGS ON MILK", &layers);
        assert!(both.contains(&"discount".to_string()), "tags were {both:?}");
        assert!(
            both.contains(&"grocery/dairy".to_string()),
            "tags were {both:?}"
        );
        assert_eq!(
            super::categorize_description("INSTANT SAVINGS ON MILK", &layers).as_deref(),
            Some("Expenses:Food:Grocery:Dairy")
        );
        // With nothing to net against, it files to its own account rather than
        // to the Expenses:FIXME it used to get.
        assert_eq!(
            super::categorize_description("2087683 TPD/969786", &layers).as_deref(),
            Some("Expenses:Discount")
        );
        // And the keyword is specific: a bare product line near those words is
        // still a product.
        assert!(!item_tags("MILK", &layers).contains(&"discount".to_string()));
    }

    #[test]
    fn uncategorized_items_are_reported_as_a_finding_with_their_index() {
        let parsed = parse_text(
            "NOFRILLS\n\
             MILK 4.00\n\
             ZZQW UNKNOWN ITEM 3.00\n\
             SUBTOTAL 7.00\n\
             TOTAL 7.00\n",
        );
        let uncategorized: Vec<_> = parsed
            .warnings
            .iter()
            .filter(|w| w.kind == ReceiptWarningKind::UncategorizedItem)
            .collect();
        assert_eq!(
            uncategorized.len(),
            1,
            "only the unknown line should be uncategorized: {:?}",
            parsed.warnings
        );
        let index = uncategorized[0]
            .after_item_index
            .expect("an uncategorized item names the item it is about");
        assert!(
            parsed.items[index].description.contains("UNKNOWN"),
            "warning points at {:?}",
            parsed.items[index]
        );
    }

    #[test]
    fn a_parsed_discount_line_is_not_a_finding() {
        // The Costco receipt that prompted this: the discount reconciles
        // exactly, so nothing about this parse deserves the user's attention —
        // and before discounts were classified, this receipt reported an
        // uncategorized item forever.
        let parsed = parse_text(
            "COSTCO\n\
             969786 PANDA COOKIE 15.99\n\
             2087683 TPD/969786 4.00-\n\
             SUBTOTAL 11.99\n\
             TOTAL 11.99\n",
        );
        assert!(
            parsed.warnings.is_empty(),
            "a clean discount receipt should report nothing: {:?}",
            parsed.warnings
        );
        let discount = parsed
            .items
            .iter()
            .find(|i| i.description.contains("TPD/"))
            .expect("the discount line is an item");
        assert_eq!(discount.price, Money::from_decimal_str("-4.00"));
        assert_eq!(discount.tags, vec!["discount"]);
        assert_eq!(discount.account.as_deref(), Some("Expenses:Discount"));
    }
}
