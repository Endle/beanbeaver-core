//! Text extraction: pairing.
use super::patterns::*;
use super::quantity::*;
use super::reconcile::*;
use super::rows::*;
use super::tokens::*;
use super::types::*;
use crate::common::ReceiptWarningKind;
use crate::money::Money;
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

/// A description row claimed by a price that was printed somewhere else.
pub(super) struct OrphanQtyPairing {
    pub(super) item: ParsedTextItem,
    /// The row the price was paired with; the caller marks it used.
    pub(super) description_line: usize,
}

/// OCR column-merge recovery: a quantity line can absorb the NEXT item's price
/// into its own text row.
///
/// FreshCo prints "2 @ 1/ $12.98 $11.19 C", where $11.19 is the price of the
/// price-less "Natrel Milk 2% 4L" line below — the right-hand price column
/// drifted up one row in OCR reading order. When a qty line carries a trailing
/// price that does NOT reconcile as its own line total (qty × unit) and the
/// next line is a bare, price-less description, that orphan price belongs to
/// that description. The qty line itself is still consumed later as a modifier
/// of the item above.
pub(super) fn orphan_qty_pairing(
    index: usize,
    line: &str,
    rows: Lines<'_>,
) -> Option<OrphanQtyPairing> {
    let prices: Vec<Money> = re_find_prices()
        .captures_iter(line)
        .filter_map(|caps| caps.get(1).and_then(|m| parse_cents(m.as_str())))
        .collect();
    // The orphan must be a genuine trailing price, not the tail of a
    // parenthetical deal ("(2/$3.50)" ends the coriander line; that 3.50 is
    // subtext, not a drifted amount).
    let trailing = extract_trailing_price_cents(line).map(|(c, _, _)| c);
    if prices.len() < 2 || trailing != prices.last().copied() {
        return None;
    }
    let orphan_cents = *prices.last().unwrap();
    let reconciles_as_own_total = parse_quantity_modifier(line)
        .map(|modifier| validate_quantity_price(orphan_cents, &modifier))
        .unwrap_or(false);
    // The downward pairing is only valid when the description above is already
    // priced, so this row has nothing left to donate upward. An unclaimed
    // description above keeps the trailing price as its own — whether the row
    // reconciles ("Broccoli (Crowns)" / "0.41 lb @ $1.98/lb  0.81") or not
    // ("HLY - Potato Chips Honey" / "(...)@3.99(1/$0.98)  5.88H", where 5.88 is
    // Honey's own price on its deal-subtext row). Under receipt-level drift even
    // a coincidentally-reconciling echo ("1 @ $1.99  1.99" where the next item
    // also costs 1.99) is the next item's price.
    let above_consumed = nearest_desc_above_consumed(rows.all, rows.used, index);
    let echo_is_drifted = rows.drift && above_consumed;
    if orphan_cents <= Money::ZERO
        || !above_consumed
        || (reconciles_as_own_total && !echo_is_drifted)
    {
        return None;
    }

    let next_idx = orphan_description_row(index, rows)?;
    let next_trimmed = rows.all[next_idx].trim();
    // Skip a line an earlier price's search already consumed — that is the
    // cross-row leak. A bare counter label ("Meat" above its mangled Chinese
    // subtext) is a real item here: the orphan price arriving from this qty row
    // is what prices it, and `is_priced_generic_item_label`'s trailing-price
    // requirement cannot see a price delivered from above.
    if rows.used[next_idx]
        || !(is_descriptive_candidate(next_trimmed) || is_generic_counter_label(next_trimmed))
    {
        return None;
    }
    let desc = strip_sale_price_subtext(&strip_leading_receipt_codes(next_trimmed));
    Some(OrphanQtyPairing {
        item: ParsedTextItem {
            category_source: desc.clone(),
            description: desc,
            price: orphan_cents,
            quantity: 1,
        },
        description_line: next_idx,
    })
}

/// The row an orphan price found on a qty line is describing.
///
/// It can sit a row or two further down when the block carries its own qty row:
/// Foody Mart prints "(<size>)@<unit>(<deal>) <orphan>" / "1 @ $2.99" /
/// "<item name>". Skip qty rows; never leap over a priced row, which is another
/// item's territory.
pub(super) fn orphan_description_row(index: usize, rows: Lines<'_>) -> Option<usize> {
    for j in (index + 1)..rows.all.len().min(index + 4) {
        let candidate = rows.all[j].trim();
        if candidate.is_empty() || looks_like_quantity_expression(candidate) {
            continue;
        }
        if line_has_trailing_price(candidate) {
            return None;
        }
        return Some(j);
    }
    None
}

/// Where a trailing price goes when the row printing it is already spoken for.
///
/// Only reachable under established drift: an unclaimed row's own trailing
/// price is its own. Stops at the first priced row rather than leaping over it,
/// because that row is the next item's territory.
pub(super) fn drifted_price_pairing(
    index: usize,
    price_cents: Money,
    rows: Lines<'_>,
) -> Option<OrphanQtyPairing> {
    for j in (index + 1)..rows.all.len().min(index + 4) {
        if rows.used[j] {
            return None;
        }
        let candidate = rows.all[j].trim();
        if line_has_trailing_price(candidate) {
            return None;
        }
        if candidate.is_empty()
            || looks_like_quantity_expression(candidate)
            || !(is_descriptive_candidate(candidate) || is_generic_counter_label(candidate))
        {
            continue;
        }
        let desc = strip_sale_price_subtext(&strip_leading_receipt_codes(candidate));
        return Some(OrphanQtyPairing {
            item: ParsedTextItem {
                category_source: desc.clone(),
                description: desc,
                price: price_cents,
                quantity: 1,
            },
            description_line: j,
        });
    }
    None
}

/// What the flag cascade decided about one row's trailing price.
///
/// These eight used to be mutable locals threaded through 200 lines of rules,
/// and that is what made the region hard to read: each rule could still change
/// what an earlier one had set, so no line of it could be understood without
/// the whole. Naming the result makes the boundary explicit — everything above
/// this struct *decides*, everything below it *acts*.
///
/// `desc_part` is part of the decision, not an input: three of the rules clear
/// it (a priced section header donates its price downward; a malformed price
/// marker keeps only the price), which is how "this row has no usable
/// description of its own" reaches the search stage.
pub(super) struct PricePlan {
    /// The description text left of the price, after the rules had their say.
    pub(super) desc_part: String,
    /// Look below the price for its description before looking above.
    pub(super) prefer_forward_desc: bool,
    /// If the forward search finds nothing, drop the price rather than
    /// back-walking — the row above is somebody else's.
    pub(super) skip_if_no_forward_desc: bool,
    /// The inline description is not usable; walk back for a real one.
    pub(super) force_backward: bool,
    /// The inline text is a size/weight fragment ("(1kg)") that belongs
    /// *appended* to whatever description the search finds, not discarded.
    pub(super) weak_inline_desc: bool,
    /// A parenthetical subtext row under established drift, whose price is the
    /// item below's. Distinct from `prefer_forward_desc` because it also stops
    /// the forward walk at the first priced row.
    pub(super) drift_paren_forward: bool,
    /// A department banner carrying a price ("&& 01-Grocery  5.59"), whose
    /// price belongs to the first real item below it.
    pub(super) is_priced_section_header: bool,
    /// The inline text is a quantity expression, not a description.
    pub(super) is_qty_expr: bool,
}

/// Stage 3 — decide how one trailing price should be paired, or that it should
/// not be.
///
/// `None` means the row is not an item price at all: a summary row, a
/// suggested-retail REG marker, a ghost promo artifact, a quantity stub, or a
/// section header whose price the item below repeats. Every one of those used
/// to be a bare `continue` in the middle of the loop.
pub(super) fn plan_price_line(
    index: usize,
    line: &str,
    price_start: usize,
    price_cents: Money,
    summary_amounts: &HashSet<Money>,
    rows: Lines<'_>,
) -> Option<PricePlan> {
    let line_upper = line.to_ascii_uppercase();
    let mut desc_part = line[..price_start].trim().to_string();
    let compact_line = re_compact_space().replace_all(&line_upper, "").to_string();
    let mut prefer_forward_desc = false;
    let mut skip_if_no_forward_desc = false;
    let previous_line = || rows.all[index - 1].as_str();

    let has_reg_marker = has_reg_price_marker(&line_upper);

    if has_reg_marker {
        if is_suggested_retail_row(line, &desc_part) {
            return None;
        }
        // Two prices on a REG row means one of them is the real one; with the
        // row above already priced, the item this row prices is below it.
        if re_find_prices().find_iter(line).count() > 1
            && index > 0
            && re_trailing_price().is_match(previous_line())
        {
            prefer_forward_desc = true;
            skip_if_no_forward_desc = true;
        }
    }

    if is_ghost_promo_row(&line_upper, &compact_line, &desc_part, index, rows) {
        return None;
    }
    if price_row_is_summary(index, &line_upper, price_cents, summary_amounts, rows) {
        return None;
    }

    let weak_inline_desc = is_weak_inline_description(&desc_part);
    let mut force_backward =
        line_upper.contains("REG$") || line_upper.contains("@REG") || weak_inline_desc;
    // Under receipt-level drift a paren-subtext row's trailing price belongs to
    // the item BELOW when the description above is already priced ("Pork Lard"
    // claimed from its qty row, so "(3 380g) 2.98" is Pak Fok's) — search
    // forward, stopping at the first priced row rather than leaping over it.
    // When the description above is still unclaimed, the price is its own
    // ("Fresh Chicken Wings" / "(WRER)  10.04") and the backward walk is right.
    let drift_paren_forward = rows.drift
        && desc_part.trim_start().starts_with('(')
        && nearest_desc_above_consumed(rows.all, rows.used, index);
    if drift_paren_forward {
        prefer_forward_desc = true;
    }
    if has_reg_marker
        && force_backward
        && index > 0
        && !previous_line().trim().is_empty()
        && line_has_trailing_price(previous_line().trim())
        && desc_part.starts_with('(')
    {
        prefer_forward_desc = true;
    }

    if !desc_part.is_empty() {
        desc_part = Regex::new(r"^\d{8,}\s*")
            .unwrap()
            .replace(&desc_part, "")
            .to_string();
    }
    let is_onsale_marker_desc = looks_like_onsale_marker(&desc_part);
    if is_onsale_marker_desc {
        prefer_forward_desc = true;
        if index > 0 && line_has_trailing_price(previous_line().trim()) {
            skip_if_no_forward_desc = true;
        }
    }

    let is_priced_section_header = !desc_part.is_empty()
        && is_section_header_text(&desc_part)
        && !is_priced_generic_item_label(&desc_part, line);
    if is_priced_section_header {
        desc_part.clear();
        if section_header_price_is_repeated(index, price_cents, rows) {
            return None;
        }
    }

    let is_malformed_price_marker = is_bare_price_marker(&desc_part);
    let is_quantity_stub = re_malformed_price_marker().is_match(&desc_part);
    let mut is_qty_expr = if !desc_part.is_empty() {
        looks_like_quantity_expression(&desc_part)
            || re_onsale_parenthetical().is_match(&desc_part)
            || is_onsale_marker_desc
    } else {
        false
    };

    if is_malformed_price_marker {
        if !malformed_marker_is_multi_buy(index, rows) {
            return None;
        }
        force_backward = true;
        desc_part.clear();
        is_qty_expr = false;
    }
    if is_quantity_stub {
        return None;
    }

    // A mangled REG-price marker (OCR ate the leading R, so "REG$15.99" became
    // "#EG15.99" or "(EG$5.99") means the trailing price is the suggested-retail
    // amount, not an item price. The inline branch already filters these via
    // `re_mangled_reg_marker`, but the search branch would back-walk and emit a
    // phantom item paired with the previous line, so suppress the whole row.
    if !desc_part.is_empty() && re_mangled_reg_marker().is_match(desc_part.trim()) {
        // Under drift with the description above already priced, the REG amount
        // lives inside the marker itself and the trailing price is the NEXT
        // item's ("(-EG4.99  2.99" above "LKS Dried Cod Fish Slice") — forward
        // it. In every other shape it is suggested retail, so keep suppressing.
        if !drift_paren_forward {
            return None;
        }
        skip_if_no_forward_desc = true;
    }

    Some(PricePlan {
        desc_part,
        prefer_forward_desc,
        skip_if_no_forward_desc,
        force_backward,
        weak_inline_desc,
        drift_paren_forward,
        is_priced_section_header,
        is_qty_expr,
    })
}

/// Whether the item below a priced department banner repeats the banner's own
/// price — in which case the banner is echoing the item, not pricing it, and
/// counting both would double the item.
///
/// Only the first non-blank row below is consulted: past it the price belongs
/// to some other item, and a summary row ends the question outright.
pub(super) fn section_header_price_is_repeated(
    index: usize,
    price_cents: Money,
    rows: Lines<'_>,
) -> bool {
    for j in (index + 1)..rows.all.len().min(index + 4) {
        let next_line = rows.all[j].trim();
        if next_line.is_empty() {
            continue;
        }
        if looks_like_summary_line(next_line) {
            return false;
        }
        return extract_trailing_price_cents(next_line)
            .is_some_and(|(next_price, _, _)| next_price == price_cents);
    }
    false
}

impl PricePlan {
    /// Whether the row's own text is the description, so no search is needed.
    ///
    /// This is the common case and the cheap one — everything below exists for
    /// the rows where it is false.
    pub(super) fn describes_itself(&self) -> bool {
        !self.desc_part.is_empty()
            && self.desc_part.len() > 2
            && !self.is_qty_expr
            && !self.force_backward
            && !self.drift_paren_forward
            && !looks_like_summary_line(self.desc_part.trim())
    }
}

/// The item a row that describes itself yields.
pub(super) fn inline_item(plan: &PricePlan, price_cents: Money) -> ParsedTextItem {
    let desc_clean = strip_sale_price_subtext(&plan.desc_part);
    let desc_clean = re_embedded_unit_price_suffix()
        .replace(&desc_clean, "")
        .trim()
        .to_string();
    ParsedTextItem {
        description: desc_clean.clone(),
        category_source: desc_clean,
        price: price_cents,
        quantity: 1,
    }
}

/// A price with no row willing to own it.
pub(super) fn unowned_price_warning(line: &str, price_cents: Money) -> DeferredTextOutcome {
    let mut message = format!("maybe missed item near price {}", price_cents);
    let context = truncated_context(line);
    if !context.is_empty() {
        message.push_str(&format!(" (context: \"{context}\")"));
    }
    DeferredTextOutcome::Warning(ReceiptWarningKind::PossibleMissedItem, message)
}

/// The quantity rows a backward walk passed on its way to a description.
///
/// Kept separate from the description itself because the walk that collects
/// them can fail to find a description, and the *next* walk's find still needs
/// them: "1 @ $2.99" above a price is that price's quantity no matter which
/// direction the name turned up in.
#[derive(Default)]
pub(super) struct QuantityContext {
    /// Quantity rows in walk order — nearest the price first.
    pub(super) info: Vec<String>,
    /// The subset that parsed as a structured modifier.
    pub(super) modifiers: Vec<QuantityModifier>,
}

/// What the four walks found.
pub(super) struct DescriptionSearch {
    /// The row that owns the price, and its cleaned text.
    pub(super) found: Option<(usize, String)>,
    pub(super) qty: QuantityContext,
    /// The plan said this price is only an item if a description turned up
    /// below it; none did, so drop it silently instead of warning. Distinct
    /// from `found: None`, which *is* worth a warning — the difference is
    /// whether the parser expected to find nothing.
    pub(super) abandoned: bool,
}

/// Stage 4 — find the row that owns a price its own row did not describe.
///
/// Four walks, and the **order is the algorithm**: each one is more permissive
/// than the last about what counts as a description, so running them in any
/// other order would let a weaker signal win. A priced department banner points
/// at the item below it; an explicit forward preference from the plan comes
/// next; the ordinary backward walk (the common case, and the only one that
/// collects quantity rows) after that; and the forward fallback last, for the
/// layouts that print the price before the name.
///
/// Every walk stops at a row an earlier price already claimed. That is what
/// keeps one item's description from leaking into another's (bugs C, H, K).
pub(super) fn find_description(
    index: usize,
    plan: &PricePlan,
    rows: Lines<'_>,
) -> DescriptionSearch {
    let abandon = |qty| DescriptionSearch {
        found: None,
        qty,
        abandoned: true,
    };

    if plan.is_priced_section_header {
        let found = describe_below_priced_header(index, rows);
        if found.is_none() {
            return abandon(QuantityContext::default());
        }
        return DescriptionSearch {
            found,
            qty: QuantityContext::default(),
            abandoned: false,
        };
    }

    let mut found = None;
    if plan.prefer_forward_desc {
        found = describe_forward(index, plan.drift_paren_forward, rows);
    }
    if plan.skip_if_no_forward_desc && found.is_none() {
        return abandon(QuantityContext::default());
    }

    let mut qty = QuantityContext::default();
    if found.is_none() {
        let walk = describe_backward(index, plan, rows);
        found = walk.found;
        qty = walk.qty;
    }
    if found.is_none()
        && !plan.prefer_forward_desc
        && (plan.desc_part.is_empty() || plan.desc_part.len() <= 3 || plan.force_backward)
    {
        found = describe_forward_fallback(index, rows);
    }

    DescriptionSearch {
        found,
        qty,
        abandoned: false,
    }
}

/// Walk 1 — the item below a priced department banner ("&& 01-Grocery  5.59").
pub(super) fn describe_below_priced_header(
    index: usize,
    rows: Lines<'_>,
) -> Option<(usize, String)> {
    for j in (index + 1)..rows.all.len().min(index + 5) {
        if rows.used[j] {
            // A used line marks the start of another item's territory; don't
            // walk past it.
            return None;
        }
        let next_line = rows.all[j].trim();
        // Under established drift the first item below a priced header often
        // carries the SECOND item's price on its own name row ("&& 01-Grocery
        // 5.59" / "S & B - Wasabi  2.68"). The header's price belongs to that
        // name regardless; skipping it would cross the whole section's pairing
        // by one.
        if rows.drift && re_trailing_price().is_match(next_line) {
            if let Some((_, _, price_start)) = extract_trailing_price_cents(next_line) {
                let head = next_line[..price_start].trim();
                let cleaned_head = strip_leading_receipt_codes(head);
                if !cleaned_head.is_empty()
                    && !is_section_header_text(&cleaned_head)
                    && alpha_ratio(&cleaned_head) >= 0.5
                {
                    return Some((j, cleaned_head));
                }
            }
        }
        if next_line.is_empty()
            || re_skip_patterns().is_match(next_line)
            || looks_like_summary_line(next_line)
            || looks_like_quantity_expression(next_line)
            || looks_like_onsale_marker(next_line)
            || re_trailing_price().is_match(next_line)
            || re_standalone_price_line().is_match(next_line)
            || re_long_digits_line().is_match(next_line)
        {
            continue;
        }
        let cleaned_next = strip_leading_receipt_codes(next_line);
        // Bare counter labels ("Meat" below a priced "&& 03-Meat" banner) are
        // the item the header's drifted price belongs to, not another banner.
        if cleaned_next.is_empty()
            || (is_section_header_text(&cleaned_next) && !is_generic_counter_label(&cleaned_next))
        {
            continue;
        }
        // The `&& <Dept> price` section-header signal is strong: the next
        // non-section line is almost always the item name even if it carries
        // trailing OCR-mangled subtext like `(125gx5)@8.99(1/$6.99)` that drags
        // the alpha ratio below 0.5. A more permissive threshold here lets
        // descriptions like "MN - Crispy Coffee Flavor 6*60g)..." (ratio 0.46)
        // pair correctly, while pure-noise lines are still rejected.
        if alpha_ratio(&cleaned_next) < 0.35 {
            continue;
        }
        return Some((j, cleaned_next));
    }
    None
}

/// Walk 2 — the plan asked to look below before looking above.
///
/// `drift_paren_forward` is the difference between skipping a priced row and
/// stopping at it: under drift the price belongs to the item immediately below,
/// so a priced row in the way means the search has already gone too far.
pub(super) fn describe_forward(
    index: usize,
    drift_paren_forward: bool,
    rows: Lines<'_>,
) -> Option<(usize, String)> {
    for j in (index + 1)..rows.all.len().min(index + 5) {
        if rows.used[j] {
            return None;
        }
        let next_line = rows.all[j].trim();
        if line_has_trailing_price(next_line) {
            if drift_paren_forward {
                return None;
            }
            continue;
        }
        if next_line.is_empty()
            || re_skip_patterns().is_match(next_line)
            || looks_like_summary_line(next_line)
            || looks_like_quantity_expression(next_line)
            || looks_like_onsale_marker(next_line)
        {
            continue;
        }
        let cleaned_next = strip_leading_receipt_codes(next_line);
        // Bare counter labels ("Meat") are items about to be priced by this row,
        // not department banners.
        if cleaned_next.is_empty()
            || (is_section_header_text(&cleaned_next) && !is_generic_counter_label(&cleaned_next))
        {
            continue;
        }
        if alpha_ratio(&cleaned_next) < 0.5 {
            continue;
        }
        return Some((j, cleaned_next));
    }
    None
}

/// Walk 3's result: a description, and the quantity rows passed to reach it.
pub(super) struct BackwardWalk {
    pub(super) found: Option<(usize, String)>,
    pub(super) qty: QuantityContext,
}

/// `12.34 G`: a price and its tax flag alone on a line, with no description.
pub(super) fn re_bare_price_with_tax_flag() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"^[\d.]+\s*{TAX_FLAG_CLASS}\s*$")).unwrap())
}

/// A bare run of 8+ digits: a barcode or member number, never a description.
pub(super) fn re_long_digit_run() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{8,}$").unwrap())
}

/// Digits and dots only — a number, not something to describe an item with.
pub(super) fn re_all_digits_and_dots() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[\d.]+$").unwrap())
}

/// Walk 3 — the ordinary case: the name is printed above its price.
///
/// This is the only walk that collects quantity context, because a quantity row
/// is printed between the name and its price ("Broccoli" / "0.41 lb @ $1.98/lb
/// 0.81") and so is passed on the way back up. It keeps collecting even when it
/// ends up finding no description, so walk 4 can still use what it saw.
pub(super) fn describe_backward(index: usize, plan: &PricePlan, rows: Lines<'_>) -> BackwardWalk {
    let mut qty = QuantityContext::default();
    let lower_bound = index.saturating_sub(5);
    for j in (lower_bound..index).rev() {
        if rows.used[j] {
            // A used line marks the end of the previous item's territory; don't
            // walk past it to grab a description belonging to an item we've
            // already paired.
            break;
        }
        let prev_line = rows.all[j].trim();
        if re_bare_price_with_tax_flag().is_match(prev_line)
            || re_long_digit_run().is_match(prev_line)
            || re_skip_patterns().is_match(prev_line)
        {
            continue;
        }
        if let Some(modifier) = parse_quantity_modifier(prev_line) {
            qty.modifiers.push(modifier);
            qty.info.push(prev_line.to_string());
            continue;
        }
        if looks_like_quantity_expression(prev_line) {
            qty.info.push(prev_line.to_string());
            continue;
        }
        if looks_like_onsale_marker(prev_line)
            || re_price_info_line().is_match(prev_line)
            || re_parenthetical_closed().is_match(prev_line)
            || (prev_line.starts_with('(') && !prev_line.contains(')'))
            || re_onsale_parenthetical().is_match(prev_line)
            || re_parenthetical_multibuy().is_match(prev_line)
            || prev_line.len() <= 3
        {
            continue;
        }
        // See patterns::SKIP_PRICED_LINES_IN_BACKWARD_DESC_SEARCH for rationale
        // and revert instructions. Limited to bare-price triggers (no qty
        // expression, no description) so OCR column-merge cases like
        // "1 @ $9.99 3.99" can still back-walk into a legitimate
        // "ITEM NAME 9.99" description line.
        if SKIP_PRICED_LINES_IN_BACKWARD_DESC_SEARCH
            && !plan.is_qty_expr
            && !plan.force_backward
            && line_has_trailing_price(prev_line)
        {
            continue;
        }

        let desc_for_ratio = strip_leading_receipt_codes(prev_line);
        if alpha_ratio(&desc_for_ratio) < 0.5 {
            continue;
        }
        if prev_line.len() > 2 && !re_all_digits_and_dots().is_match(prev_line) {
            let cleaned_prev = strip_leading_receipt_codes(prev_line);
            if !cleaned_prev.is_empty() {
                return BackwardWalk {
                    found: Some((j, cleaned_prev)),
                    qty,
                };
            }
        }
    }
    BackwardWalk { found: None, qty }
}

/// Walk 4 — the price came before the name.
///
/// Reached when the price row has no usable description of its own (empty, very
/// short, or a weak parenthetical like "(1kg)") and the backward walk found
/// nothing. Foody Mart-style layouts print it this way.
pub(super) fn describe_forward_fallback(index: usize, rows: Lines<'_>) -> Option<(usize, String)> {
    for j in (index + 1)..rows.all.len().min(index + 3) {
        if rows.used[j] {
            return None;
        }
        let next_line = rows.all[j].trim();
        if next_line.is_empty()
            || re_skip_patterns().is_match(next_line)
            || looks_like_summary_line(next_line)
            || looks_like_quantity_expression(next_line)
            || looks_like_onsale_marker(next_line)
            || line_has_trailing_price(next_line)
            || re_standalone_price_line().is_match(next_line)
            || re_long_digits_line().is_match(next_line)
        {
            continue;
        }
        let cleaned_next = strip_leading_receipt_codes(next_line);
        // Treat unpriced "Meat" / "Bakery" lines as legitimate descriptions even
        // though those words are also in the section-name table — that's how
        // Asian-grocery receipts label the items.
        let is_generic_priced_label = matches!(
            cleaned_next.trim().to_ascii_uppercase().as_str(),
            "MEAT" | "BAKERY"
        );
        if cleaned_next.is_empty()
            || (is_section_header_text(&cleaned_next) && !is_generic_priced_label)
        {
            continue;
        }
        if alpha_ratio(&cleaned_next) < 0.5 {
            continue;
        }
        return Some((j, cleaned_next));
    }
    None
}

/// The item a searched-out description yields.
///
/// The quantity context decides between two shapes: a modifier whose arithmetic
/// checks out against the price becomes a real `quantity` (and a weight suffix),
/// while one that does not is appended as text instead. Keeping an unreconciled
/// quantity as prose rather than as a number is deliberate — a wrong quantity
/// silently corrupts the ledger, a parenthetical is visible.
pub(super) fn searched_item(
    all_lines: &[String],
    desc_line: usize,
    desc_text: String,
    price_cents: Money,
    plan: &PricePlan,
    qty: &QuantityContext,
) -> ParsedTextItem {
    let mut found_desc_value = merge_description_context(all_lines, &desc_text, desc_line);
    if plan.weak_inline_desc {
        found_desc_value = format!("{found_desc_value} {}", plan.desc_part)
            .trim()
            .to_string();
    }
    let mut quantity = 1;
    let mut description_suffix = String::new();
    let as_text = || {
        format!(
            " ({})",
            qty.info
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    match qty.modifiers.first() {
        Some(modifier) if validate_quantity_price(price_cents, modifier) => {
            quantity = modifier.quantity;
            if let Some(weight_text) = &modifier.weight_text {
                description_suffix = format!(" ({weight_text} lb)");
            }
        }
        _ if !qty.info.is_empty() => description_suffix = as_text(),
        _ => {}
    }

    let cleaned_desc = strip_sale_price_subtext(&found_desc_value);
    ParsedTextItem {
        category_source: cleaned_desc.clone(),
        description: format!("{cleaned_desc}{description_suffix}"),
        price: price_cents,
        quantity,
    }
}
