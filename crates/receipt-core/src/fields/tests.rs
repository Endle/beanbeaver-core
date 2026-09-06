use super::amounts::*;
use super::dates::*;
use super::prices::*;
use super::tenders::*;

#[test]
fn tax_equal_to_the_subtotal_is_rederived() {
    // Pharmasave: the summary column drifted up a row, so HST claimed the
    // subtotal's 10.79 and TOTAL claimed the tax's 1.40.
    assert_eq!(reconcile_tax(Some(1079), Some(1079), 1219), Some(140));
}

#[test]
fn tax_equal_to_the_total_is_rederived() {
    // Walmart: "HST" merged onto the TOTAL row as "HST TOTAL $58.94".
    assert_eq!(reconcile_tax(Some(5894), Some(5380), 5894), Some(514));
}

#[test]
fn implausibly_large_tax_is_rederived() {
    // Half the subtotal is not a Canadian tax rate.
    assert_eq!(reconcile_tax(Some(5000), Some(10000), 11300), Some(1300));
}

#[test]
fn zero_tax_under_a_larger_total_is_rederived() {
    // Foody Mart 2026-08-22: OCR read "HST 1.82" as "11:82" — no parseable
    // amount — so the summed buckets returned the hst5% row's 0.00 while the
    // receipt charged 1.82 on its hot-food line.
    assert_eq!(reconcile_tax(Some(0), Some(11756), 11938), Some(182));
}

#[test]
fn zero_tax_on_an_untaxed_receipt_is_left_alone() {
    // The ordinary zero-rated grocery basket: 0.00 tax printed, and the total
    // agrees with the subtotal. There is nothing to derive and nothing wrong.
    assert_eq!(reconcile_tax(Some(0), Some(4210), 4210), Some(0));
}

#[test]
fn zero_tax_is_left_alone_when_the_gap_is_too_large_to_be_tax() {
    // A gap of 40% of the subtotal is a mis-read total or a missing summary
    // line, not a tax rate — deriving from it would invent an amount.
    assert_eq!(reconcile_tax(Some(0), Some(1000), 1400), Some(0));
}

#[test]
fn a_repaired_tax_is_reported_as_repaired() {
    let lines = vec![
        "Sub Total 117.56".to_string(),
        "HST 11:82".to_string(),
        "hst5% 0.00".to_string(),
        "Total after Tax 119.38".to_string(),
    ];
    let reading = extract_summary_reconciled(&lines, 11938).tax;
    assert_eq!(reading.printed_cents, Some(0));
    assert_eq!(reading.cents, Some(182));
    assert!(reading.was_repaired());
}

#[test]
fn an_untouched_tax_is_not_reported_as_repaired() {
    let lines = vec![
        "Sub Total 181.32".to_string(),
        "HST 2.36".to_string(),
        "hst5% 0.09".to_string(),
        "Total after Tax 183.77".to_string(),
    ];
    let reading = extract_summary_reconciled(&lines, 18377).tax;
    assert_eq!(reading.cents, Some(245));
    assert!(!reading.was_repaired());
}

#[test]
fn summary_block_off_by_a_row_is_re_read_from_the_trailer_echo() {
    // costco/2026-08-26_costco_737_56. SUBTOTAL merged onto the DEPOSIT VL
    // row and took its 4.00, so TAX took the subtotal's 707.54 and TOTAL the
    // tax's 30.02. `reconcile_tax` alone cannot save this: it derives
    // 737.56 - 4.00 = 733.56, which is not a plausible tax either, because
    // the subtotal it derives from is wrong too.
    let lines = vec![
        "SUBTOTAL DEPOSIT VL 4.00".to_string(),
        "TAX 707.54".to_string(),
        "***TOTAL 30.02".to_string(),
        "737.56".to_string(),
        "MasterCard 737.56".to_string(),
        "P (H)HST 13% 30.02".to_string(),
        "TOTAL TAX 30.02".to_string(),
    ];
    let reading = extract_summary_reconciled(&lines, 73756);
    assert_eq!(reading.subtotal_cents, Some(70754));
    assert_eq!(reading.tax.cents, Some(3002));
    assert!(reading.shift_repaired());
}

#[test]
fn summary_block_off_by_a_row_is_re_read_from_the_identity() {
    // costco/2026-04-26_costco_173_15: the SUBTCTAL row absorbed two
    // amounts and the parser took the second. Its trailer echo is mangled
    // ("(HOHST 13% 12" carries no readable amount), so only the identity
    // search can place these — and exactly one pair of the printed amounts
    // satisfies it.
    let lines = vec![
        "SUBTCTAL 159.08 14.07".to_string(),
        "TAX 173.15".to_string(),
        "**** TOTAL".to_string(),
        "AMOUNT: 173.15".to_string(),
        "(HOHST 13% 12".to_string(),
    ];
    let reading = extract_summary_reconciled(&lines, 17315);
    assert_eq!(reading.subtotal_cents, Some(15908));
    assert_eq!(reading.tax.cents, Some(1407));
}

#[test]
fn a_split_tender_summing_to_the_total_does_not_pass_for_a_summary_block() {
    // costco/2026-07-08_costco_112_95 pays with a $100.00 gift card and
    // $12.95 on a card, and those sum to the total exactly as subtotal plus
    // tax does. Arithmetic alone cannot separate the two readings, so the
    // identity search must decline — the echo is what settles this receipt.
    let lines = vec![
        "TOTAL NUMBER OF ITEMS SOLD = 9 104.77".to_string(),
        "SUBTOTAL 8.18".to_string(),
        "TAX 112.95".to_string(),
        "**** TOTAL".to_string(),
        "Shop Card AMOUNT: $100.00 Resp: Approved".to_string(),
        "Shop Card 100.00".to_string(),
        "AMOUNT: 12.95".to_string(),
    ];
    let ambiguous = extract_summary_reconciled(&lines, 11295);
    assert_eq!(ambiguous.subtotal_cents, Some(818));
    assert!(!ambiguous.shift_repaired());

    let mut with_echo = lines.clone();
    with_echo.push("P (H)HST 13% 8.18".to_string());
    let reading = extract_summary_reconciled(&with_echo, 11295);
    assert_eq!(reading.subtotal_cents, Some(10477));
    assert_eq!(reading.tax.cents, Some(818));
}

#[test]
fn a_consistent_summary_block_is_never_reshuffled() {
    // The identity holds, so nothing here is impossible and no repair may
    // fire — even though 70.32 + 3.90 is not the only pair on the receipt.
    let lines = vec![
        "SUBTOTAL 70.32".to_string(),
        "TAX 3.90".to_string(),
        "**** TOTAL 74.22".to_string(),
        "P (H)HST 13% 3.90".to_string(),
    ];
    let reading = extract_summary_reconciled(&lines, 7422);
    assert_eq!(reading.subtotal_cents, Some(7032));
    assert_eq!(reading.tax.cents, Some(390));
    assert!(!reading.shift_repaired());
}

#[test]
fn a_deposit_breaking_the_identity_is_not_a_shift() {
    // subtotal + tax falls short of the total by a bottle deposit charged
    // after the subtotal. The tax is plausible, so this is a receipt doing
    // something legitimate, not a block that slipped.
    let lines = vec![
        "SUBTOTAL 10.00".to_string(),
        "TAX 1.30".to_string(),
        "DEPOSIT 0.20".to_string(),
        "**** TOTAL 11.50".to_string(),
    ];
    let reading = extract_summary_reconciled(&lines, 1150);
    assert_eq!(reading.subtotal_cents, Some(1000));
    assert_eq!(reading.tax.cents, Some(130));
    assert!(!reading.shift_repaired());
}

#[test]
fn a_derived_subtotal_the_receipt_never_printed_is_refused() {
    // The echo says 30.02, which would imply a 707.54 subtotal — but no such
    // amount is printed here. The repair re-assigns figures the receipt
    // carries; it never invents one.
    let lines = vec![
        "SUBTOTAL 4.00".to_string(),
        "TAX 900.00".to_string(),
        "**** TOTAL".to_string(),
        "P (H)HST 13% 30.02".to_string(),
    ];
    let reading = extract_summary_reconciled(&lines, 73756);
    assert_eq!(reading.subtotal_cents, Some(400));
    assert!(!reading.shift_repaired());
}

#[test]
fn merely_inconsistent_tax_is_left_alone() {
    // subtotal + tax != total by 10c — a deposit or bottle fee, not a
    // mis-paired label. Rewriting these is exactly what this must not do.
    assert_eq!(reconcile_tax(Some(130), Some(1000), 1140), Some(130));
}

#[test]
fn tax_is_left_alone_when_the_derived_value_is_implausible() {
    // Contradictory tax, but total - subtotal is 60% of the subtotal, so the
    // arithmetic offers nothing better to swap in.
    assert_eq!(reconcile_tax(Some(1000), Some(1000), 1600), Some(1000));
}

#[test]
fn tax_is_left_alone_without_a_subtotal_to_check_against() {
    assert_eq!(reconcile_tax(Some(5894), None, 5894), Some(5894));
    assert_eq!(reconcile_tax(None, Some(5380), 5894), None);
}

#[test]
fn rate_suffixed_tax_label_is_read_as_a_tax_row() {
    // Foody Mart 2026-08-07 prints its 5% bucket as "hst5%", with no space
    // before the rate, so the label's trailing word boundary never landed and
    // the row read as untaxed text. The 0.20 then reached the ledger as an
    // unaccounted FIXME.
    let lines = vec![
        "Sub Total 81.76".to_string(),
        "HST 0.00".to_string(),
        "hst5% 0.20".to_string(),
        "Total after Tax 81.96".to_string(),
    ];
    assert_eq!(extract_tax(&lines), Some(20));
}

#[test]
fn split_tax_buckets_are_summed() {
    // Bestco 2026-06-25 charges both buckets: 181.32 + 2.36 + 0.09 = 183.77.
    // Reading either row alone loses the other.
    let lines = vec![
        "Sub Total 181.32".to_string(),
        "HST 2.36".to_string(),
        "hst5% 0.09".to_string(),
        "Total after Tax 183.77".to_string(),
    ];
    assert_eq!(extract_tax(&lines), Some(245));
}

#[test]
fn tax_restated_below_the_total_is_not_double_counted() {
    // Costco prints the tax twice — once in the summary block, then again in
    // the trailer that breaks it down by code. Both rows say 1.04, and the
    // receipt's tax is 1.04, not 2.08. Only the rows above the total line are
    // components of it.
    let lines = vec![
        "SUBTOTAL 70.23".to_string(),
        "TAX 1.04".to_string(),
        "**** TOTAL 71.27".to_string(),
        "P (H)HST 13% 1.04".to_string(),
        "TOTAL TAX 1.04".to_string(),
    ];
    assert_eq!(extract_tax(&lines), Some(104));
}

#[test]
fn tax_registration_number_in_the_header_is_not_a_tax_row() {
    // "HST#821366291RT0001" sits above the subtotal, outside the window, and
    // carries no amount besides — neither reason alone should be relied on.
    let lines = vec![
        "(905)305-9866 HST#821366291RT0001".to_string(),
        "Sub Total 81.76".to_string(),
        "hst5% 0.20".to_string(),
        "Total after Tax 81.96".to_string(),
    ];
    assert_eq!(extract_tax(&lines), Some(20));
}

#[test]
fn comma_read_as_decimal_point_still_yields_a_total() {
    // Foody Mart 2026-07-29 printed both amounts identically, but OCR read
    // only the grand-total row's point as a comma: "Sub Total 110.05" /
    // "Total after Tax 110,05". The total row parsed to nothing, so the
    // credit-card posting was written as 0.00.
    let lines = vec![
        "Sub Total 110.05".to_string(),
        "Total after Tax 110,05".to_string(),
    ];
    assert_eq!(extract_total(&lines), 11005);
}

#[test]
fn thousands_separator_is_not_rewritten_as_a_decimal_point() {
    // The guard that makes the comma rule safe, asserted against *this*
    // module's copy of `normalize_decimal_spacing` — it is the copy that
    // drifted, so testing the shared behavior elsewhere would not have
    // caught it. Whether the extractor then reads the leading "1," is a
    // separate, pre-existing limitation: it takes the "299.99" tail.
    assert_eq!(
        normalize_decimal_spacing("TOTAL 1,299.99"),
        "TOTAL 1,299.99"
    );
    assert_eq!(normalize_decimal_spacing("Anytown, ON"), "Anytown, ON");
}

#[test]
fn date_parses_day_first_hyphenated_month_name() {
    // Jin Lian Food / Clover format: "22-May-2026 3:22:42p.m."
    let lines = vec!["22-May-2026 3:22:42p.m.".to_string()];
    let parsed = extract_date(&lines, "", 2026).expect("date should parse");
    assert_eq!(parsed.ymd(), (2026, 5, 22));
}

#[test]
fn return_deadline_does_not_outrank_the_transaction_date() {
    let lines = vec![
        "Last Valid Date for Return of Product Is:".to_string(),
        "Date limite pour retour de produits".to_string(),
        "26 SEP 2026".to_string(),
        "V124.04 27 AUG 2026 04:11PM".to_string(),
    ];
    let parsed = extract_date(&lines, "", 2026).expect("transaction date should parse");
    assert_eq!(parsed.to_string(), "2026-08-27");
}

#[test]
fn a_return_deadline_alone_is_not_a_purchase_date() {
    let lines = vec![
        "Last Valid Date for Return of Product Is:".to_string(),
        "Date limite pour retour de produits".to_string(),
        "26 SEP 2026".to_string(),
    ];
    assert_eq!(extract_date(&lines, "", 2026), None);
}

#[test]
fn date_parses_dotted_month_abbreviation() {
    // Clover also prints an abbreviation period: "02-Apr.-2026 2:27:39p.m."
    let lines = vec!["02-Apr.-2026 2:27:39p.m.".to_string()];
    let parsed = extract_date(&lines, "", 2026).expect("date should parse");
    assert_eq!(parsed.ymd(), (2026, 4, 2));
}

#[test]
fn date_hint_survives_ocr_damage_to_the_datetime_suffix() {
    // No Frills prints "DateTime: 26/08/02"; PP-OCRv5 read it as "Datelime".
    // The hint is what admits the year-first reading, so losing it to a
    // single glyph moved the date 24 years: 2026-08-02 -> 2002-08-26.
    for label in ["DateTime", "Datelime", "DATETIME", "Dateiime", "Date"] {
        let lines = vec![format!("{label}: 26/08/02 15:48:10")];
        let parsed = extract_date(&lines, "", 2026)
            .unwrap_or_else(|| panic!("date should parse for label {label:?}"));
        assert_eq!(
            parsed.ymd(),
            (2026, 8, 2),
            "label {label:?} should read 26/08/02 as year-first"
        );
    }
}

#[test]
fn date_hint_does_not_fire_inside_a_longer_word() {
    // `\bDATE` still needs a boundary before the prefix, so "UPDATE" is not
    // date context and the year-first reading stays gated.
    let lines = vec!["UPDATED: 26/08/02".to_string()];
    let parsed = extract_date(&lines, "", 2026).expect("date should parse");
    assert_ne!(parsed.ymd(), (2026, 8, 2));
}

#[test]
fn subtotal_tolerates_costco_subtctal_ocr_typo() {
    // Costco "SUBTOTAL" OCR'd as "SUBTCTAL" (inner O → C).
    let lines = vec![
        "***END OF PRE-SCANNED ITEMS***".to_string(),
        "SUBTCTAL 159.08".to_string(),
        "TAX 14.07".to_string(),
    ];

    assert_eq!(extract_subtotal(&lines), Some(15_908));
}

#[test]
fn bare_total_takes_tax_row_amount_when_it_exceeds_the_subtotal() {
    // Costco 2026-07-08_costco_112_95: up-leaned line grouping left the
    // TOTAL row bare and put the grand total on the TAX row (and the tax
    // on SUBTOTAL). A real tax can never exceed the subtotal amount.
    let lines = vec![
        "TOTAL NUMBER OF ITEMS SOLD = 9 104.77".to_string(),
        "SUBTOTAL 8.18".to_string(),
        "TAX 112.95".to_string(),
        "**** TOTAL".to_string(),
        "XXXXXXXXXXXX7735".to_string(),
    ];

    assert_eq!(extract_total(&lines), 11_295);
}

#[test]
fn total_row_holding_a_split_off_discount_label_is_not_the_grand_total() {
    // Costco 2026-03-07_costco_466_68: once line grouping shifts, the
    // "TOTAL DISCOUNT(S) $9.00" row can split, stranding a bare
    // "DISCOUNT(S)" above a "TOTAL $ 9.00" that is really the discount.
    let lines = vec![
        "AMOUNT: 441.68".to_string(),
        "466.68".to_string(),
        "NUMBER OF".to_string(),
        "TOTAL ITEMS SOLD".to_string(),
        "DISCOUNT(S)".to_string(),
        "TOTAL $ 9.00".to_string(),
    ];

    assert_ne!(extract_total(&lines), 900);
}

#[test]
fn a_savings_summary_is_not_the_grand_total_however_it_is_worded() {
    // Food Basics 2026-07-31: the savings block is the last thing on the
    // receipt above the payment slip, and the scan runs upward, so
    // "Total of your savings" was reached before the real "TOTAL 6.96".
    // The words are not adjacent, so the old `TOTAL SAVINGS` literal missed
    // it and a $6.96 receipt reported $6.73.
    let lines = vec![
        "SUBTOTAL 6.96".to_string(),
        "TOTAL 6.96".to_string(),
        "CREDIT CR 6.96".to_string(),
        "Total number of items sold = 11".to_string(),
        "****** Your savings today ******".to_string(),
        "Promotional discounts 6.73".to_string(),
        "Total of your savings 6.73".to_string(),
    ];

    assert_eq!(extract_total(&lines), 696);
}

#[test]
fn the_savings_guard_does_not_swallow_a_real_total() {
    // It must stay a *savings* rule: an ordinary grand total that happens
    // to sit under a savings line is still the total.
    let lines = vec![
        "Your Total Savings 6.73".to_string(),
        "TOTAL 95.00".to_string(),
    ];

    assert_eq!(extract_total(&lines), 9_500);
}

#[test]
fn discount_row_carrying_its_own_amount_still_allows_a_real_total() {
    // The guard above must stay narrow: a discount line that has its own
    // number is an ordinary row, and the total after it is genuine.
    let lines = vec!["DISCOUNT 5.00".to_string(), "TOTAL 95.00".to_string()];

    assert_eq!(extract_total(&lines), 9_500);
}

#[test]
fn total_row_carrying_a_tender_amount_is_settled_by_subtotal_plus_tax() {
    // FreshCo unknown-date_freshco_157_38: the price column leans up, so
    // the Corp Gift Card tender's 116.24 lands on the TOTAL row. The
    // trailing-price pick takes the tender; subtotal + tax says otherwise,
    // and 157.38 is right there on the same line.
    let lines = vec![
        "SUBTOTAL $146.48".to_string(),
        "TOTAL TAX $10.90".to_string(),
        "TOTAL $157.38 $116.24".to_string(),
        "Corp Gift Card TENDER".to_string(),
    ];

    assert_eq!(extract_total(&lines), 15_738);
}

#[test]
fn total_row_is_left_alone_when_the_sum_is_not_printed_on_it() {
    // The override needs the arithmetic to be corroborated *on that row*.
    // Here subtotal + tax = 157.38 but the row carries no such amount, so
    // the ordinary pick stands — receipts whose total legitimately differs
    // from subtotal + tax (fees, rounding) must not be rewritten.
    let lines = vec![
        "SUBTOTAL $146.48".to_string(),
        "TOTAL TAX $10.90".to_string(),
        "TOTAL $160.00".to_string(),
    ];

    assert_eq!(extract_total(&lines), 16_000);
}

#[test]
fn bare_total_still_ignores_a_plausible_tax_row_above() {
    // The TAX guard must keep holding when the tax amount is smaller than
    // the subtotal (the normal case for a bare TOTAL line).
    let lines = vec![
        "SUBTOTAL 104.77".to_string(),
        "TAX 8.18".to_string(),
        "**** TOTAL".to_string(),
    ];

    assert_eq!(extract_total(&lines), 0);
}

#[test]
fn total_prefers_single_tender_when_change_due_is_zero() {
    // Pharmasave 2026-07-07_pharmasave_12_19: line grouping handed the
    // TOTAL row the HST amount. With CHANGE DUE at $0.00 the lone VISA
    // tender is the grand total by definition.
    let lines = vec![
        "SUBTOTAL".to_string(),
        "HST $10.79".to_string(),
        "TOTAL $1.40".to_string(),
        "VISA $12.19".to_string(),
        "CHANGE DUE $12.19 $0.00".to_string(),
    ];

    assert_eq!(extract_total(&lines), 1_219);
}

#[test]
fn zero_change_does_not_promote_one_tender_of_a_split() {
    // The relaxation above says "nothing handed back ⇒ that tender is the
    // whole total". True of one instrument; false of two. Cash is not in
    // `payment_amounts`, so the VISA portion looked like the entire charge
    // and was adopted: 23.41 reported as the total of a 33.41 receipt, with
    // nothing to say so. Falling through to the mis-grouped 2.41 is the
    // correct outcome here — wrong, but wrong *loudly*: it disagrees with
    // both the subtotal and the tender block, and `TenderMismatch` fires.
    let lines = vec![
        "SUBTOTAL 31.42".to_string(),
        "HST 2.41".to_string(),
        "TOTAL 2.41".to_string(),
        "VISA 23.41".to_string(),
        "CASH 10.00".to_string(),
        "CHANGE 0.00".to_string(),
    ];
    assert_eq!(extract_total(&lines), 241);
}

#[test]
fn ten_dollars_change_is_not_zero_change() {
    // `"10.00".ends_with("0.00")` is true, so the old suffix test read every
    // whole-ten-dollar change amount as zero — the ordinary cash case, and
    // precisely the population the two-line rule protects.
    let lines = vec![
        "SUBTOTAL 31.42".to_string(),
        "HST 2.41".to_string(),
        "TOTAL 2.41".to_string(),
        "VISA 23.41".to_string(),
        "CHANGE 10.00".to_string(),
    ];
    assert_eq!(extract_total(&lines), 241);
}

#[test]
fn total_picks_max_when_total_and_tax_share_a_line() {
    // OCR collapsed Freshco's two-column "TOTAL | TOTAL TAX | $74.55 | $1.82"
    // row into a single line. The trailing price is the tax; the actual
    // total is the larger value.
    let lines = vec![
        "SUBTOTAL $72.73".to_string(),
        "TOTAL TOTAL TAX $74.55 $1.82".to_string(),
    ];

    assert_eq!(extract_total(&lines), 7_455);
}

#[test]
fn total_reconciles_to_corroborated_charge_when_label_mispaired() {
    // On-device box-position artifact: the TOTAL label paired with the tax
    // row (20.14); the real total (245.87) is orphaned but corroborated by
    // the card tender and the AMOUNT: echo. Reconciliation recovers it.
    let lines = vec![
        "TOTAL 20.14".to_string(),
        "245.87".to_string(),
        "AMOUNT: 245.87".to_string(),
        "MasterCard 245.87".to_string(),
    ];
    assert_eq!(extract_total(&lines), 24_587);
}

#[test]
fn total_reconciles_from_credit_tn_echo_when_total_digits_garbled() {
    // No Frills 2026-04-23_nofrills_11_15: bleed-through from the reverse
    // side garbles the digits on the SUBTOTAL/TOTAL rows ("1 1.1 5" /
    // "1 11 5"), so the label scan yields 0. The clean amount survives on
    // the card slip's "Account: VISA" line and its "CREDIT TN" echo —
    // two corroborating payment lines.
    let lines = vec![
        "SUBTOTALbemutord yom eaib1 1.1 5".to_string(),
        "TOTAL dtiw eeorotuqto yobA nir1 11 5".to_string(),
        "yob Al oto ylno egnorox3.gnigoxbq bnd apot".to_string(),
        "Trans.Type: PURCHASE qqo anoitqeoxe amo2".to_string(),
        "Account: VISA CAD$ 11. 15".to_string(),
        "Card Type: CREDIT".to_string(),
        "CREDIT TN 11.15".to_string(),
    ];
    assert_eq!(extract_total(&lines), 1_115);
}

#[test]
fn total_reconciliation_leaves_correct_total_unchanged() {
    // Correctly paired: the candidate already equals the charged amount, so
    // reconciliation must not fire (this is the desktop/cached-parity guard).
    let lines = vec![
        "TOTAL 50.00".to_string(),
        "AMOUNT: 50.00".to_string(),
        "VISA 50.00".to_string(),
    ];
    assert_eq!(extract_total(&lines), 5_000);
}

#[test]
fn total_reconciliation_ignores_split_tender_card_portion() {
    // Split tender: the real total (50.00) exceeds the card portion (30.00),
    // so the corroborated card+AMOUNT amount must NOT override it.
    let lines = vec![
        "TOTAL 50.00".to_string(),
        "GIFT CARD 20.00".to_string(),
        "AMOUNT: 30.00".to_string(),
        "VISA 30.00".to_string(),
    ];
    assert_eq!(extract_total(&lines), 5_000);
}

#[test]
fn total_reconciliation_holds_on_real_costco_split_tender() {
    // Real Costco split tender (2026-03-07, $466.68 = $25.00 Shop Card +
    // $441.68 MasterCard). The receipt carries two "AMOUNT:" echoes plus the
    // card line, but neither charged amount exceeds the printed total, so the
    // `> candidate` guard must leave 466.68 intact. Exercises the two-AMOUNT,
    // gift-card-classified shape the synthetic split-tender case above misses.
    let lines = vec![
        "TOTAL 466.68".to_string(),
        "Shop Card 25.00".to_string(),
        "AMOUNT: $25.00".to_string(),
        "MASTERCARD".to_string(),
        "AMOUNT: 441.68".to_string(),
        "MasterCard 441.68".to_string(),
        "CHANGE 0.00".to_string(),
    ];
    assert_eq!(extract_total(&lines), 46_668);
}

#[test]
fn total_reconciliation_holds_on_real_costco_single_tender() {
    // Real Costco desktop OCR (2026-03-05): TOTAL is already correctly paired
    // and the AMOUNT:/MasterCard echoes equal it, so reconciliation never
    // fires (charge == candidate, not >). Desktop/cached-parity guard.
    let lines = vec![
        "SUBTOTAL 225.73".to_string(),
        "TAX 20.14".to_string(),
        "TOTAL 245.87".to_string(),
        "AMOUNT: 245.87".to_string(),
        "MasterCard 245.87".to_string(),
        "CHANGE 0.00".to_string(),
    ];
    assert_eq!(extract_total(&lines), 24_587);
}

#[test]
fn total_after_tax_zero_prefers_following_standalone_amount() {
    let lines = vec![
        "Item Count: 33".to_string(),
        "Sub Total 153.55".to_string(),
        "HST".to_string(),
        "hst5% 0.00".to_string(),
        "Total after Tax 0.00".to_string(),
        "153.55".to_string(),
        "Credit Card".to_string(),
        "153.55".to_string(),
    ];

    assert_eq!(extract_total(&lines), 15_355);
}

#[test]
fn total_and_tax_survive_ocr_mangled_total_after_tax_label() {
    // Foody Mart 2026 receipt footer where OCR mangled "Total" -> "lotal":
    //   Sub Total 159.41 / HST 4.54 / list5% 0.00 / lotal after Tax 163.95
    // The grand total is the "after tax" line (163.95) even though "Total"
    // is unreadable, the tax is the HST line (4.54) not the after-tax
    // amount, and the spaced "Sub Total" must not be taken as the total.
    let lines = vec![
        "1iem Count: 40".to_string(),
        "Sub Total 159.41".to_string(),
        "HST 4.54".to_string(),
        "list5% 0.00".to_string(),
        "lotal after Tax 163.95".to_string(),
        "Credit Cand 163.95".to_string(),
    ];

    assert_eq!(extract_total(&lines), 16_395);
    assert_eq!(extract_tax(&lines), Some(454));
    assert_eq!(extract_subtotal(&lines), Some(15_941));
}

#[test]
fn tenders_split_costco_shop_card_and_mastercard() {
    // Costco prints: AMOUNT: $25.00 / REMAINING BALANCE: $0.00 / Shop Card 25.00
    // / XXXXXXXXXXXX4385 / ACCT: MASTERCARD / (next line) 441.68.
    let lines = vec![
        "TOTAL".to_string(),
        "466.68".to_string(),
        "AMOUNT: $25.00".to_string(),
        "REMAINING BALANCE: $0.00".to_string(),
        "Shop Card".to_string(),
        "25.00".to_string(),
        "XXXXXXXXXXXX4385".to_string(),
        "MASTERCARD".to_string(),
        "441.68".to_string(),
    ];

    let tenders = extract_tenders(&lines);
    assert!(tenders_reconcile(&lines, &tenders, 46_668));
    assert_eq!(tenders.len(), 2);
    assert_eq!(tenders[0].kind, "gift_card");
    assert_eq!(tenders[0].amount_cents, 2_500);
    assert_eq!(tenders[0].raw_label, "Shop Card");
    assert_eq!(tenders[1].kind, "card");
    assert_eq!(tenders[1].amount_cents, 44_168);
    assert_eq!(tenders[1].raw_label, "MASTERCARD");
}

#[test]
fn tenders_are_reported_even_when_the_sum_does_not_reconcile() {
    let lines = vec!["TOTAL 50.00".to_string(), "MASTERCARD 30.00".to_string()];
    // Only 30 of 50 covered. The tender line is still what the receipt
    // printed, so it is still reported — discarding it was how a misread
    // amount became indistinguishable from a receipt with no payment block.
    let tenders = extract_tenders(&lines);
    assert_eq!(tenders.len(), 1);
    assert_eq!(tenders[0].amount_cents, 3_000);
    assert!(!tenders_reconcile(&lines, &tenders, 5_000));
}

#[test]
fn a_one_cent_tender_gap_does_not_reconcile() {
    // The old $0.05 tolerance called this reconciled and emitted both
    // tenders as postings, so the payment side summed to 96.64 against an
    // item side summing to 96.65 and beancount rejected the entry. Every
    // amount in a payment block is printed to the cent: a cent off is a
    // misread digit, not rounding.
    let lines = vec![
        "Total 96.65".to_string(),
        "Gift Card 30.05".to_string(),
        "Gift Card 66.59".to_string(),
    ];
    let tenders = extract_tenders(&lines);
    assert_eq!(tenders.len(), 2);
    assert!(!tenders_reconcile(&lines, &tenders, 9_665));
}

#[test]
fn lcbo_split_gift_cards_reconcile() {
    // The shape this must keep working: LCBO pays one slip from two gift
    // cards, and the amounts partition the total instead of echoing it.
    let lines = vec![
        "Total 39.90".to_string(),
        "Deposit (DEP) 0.40".to_string(),
        "Gift Card 18.10".to_string(),
        "608835xxxxx2424684x EXP:NONE".to_string(),
        "AUTHOR.#:607022550 BAL: 0.00".to_string(),
        "Gift Card 21.80".to_string(),
    ];
    let tenders = extract_tenders(&lines);
    assert_eq!(tenders.len(), 2);
    assert!(tenders_reconcile(&lines, &tenders, 3_990));
}

#[test]
fn an_empty_tender_block_is_not_a_disagreement() {
    // Most receipts print no payment block at all; that is silence, not a
    // contradiction, and must not warn.
    let lines = vec!["TOTAL 50.00".to_string()];
    let tenders = extract_tenders(&lines);
    assert!(tenders.is_empty());
    assert!(tenders_reconcile(&lines, &tenders, 5_000));
}

#[test]
fn tenders_ignores_change_and_cash_back_lines() {
    let lines = vec![
        "TOTAL 20.00".to_string(),
        "CASH 25.00".to_string(),
        "CASH BACK 0.00".to_string(),
        "CHANGE 5.00".to_string(),
    ];
    let tenders = extract_tenders(&lines);
    // Only the CASH line is a tender; CASH BACK and CHANGE are not.
    assert_eq!(tenders.len(), 1);
    assert_eq!(tenders[0].amount_cents, 2_500);
    // ...and $25 tendered against a $20 total is not a disagreement once
    // the $5 change is netted off. This used to pass for the wrong reason:
    // the old tolerance check saw 25 vs 20, gave up, and returned nothing.
    assert!(tenders_reconcile(&lines, &tenders, 2_000));
}

#[test]
fn change_is_the_last_amount_on_a_merged_row() {
    // Costco's customer copy prints the card charge and the change on
    // consecutive rows, and line grouping merges them. Reading the FIRST
    // amount took 441.68 as change handed back, so the net tendered went
    // negative (-416.68) and the warning reported a $883.36 discrepancy on
    // a receipt that is merely missing one tender line.
    let lines = vec![
        "TOTAL 466.68".to_string(),
        "Shop Card 25.00".to_string(),
        "MasterCard 441.68 CHANGE 0.00".to_string(),
    ];
    assert_eq!(extract_change(&lines), 0);
}

#[test]
fn exchange_in_a_return_policy_is_not_a_change_line() {
    // `contains("CHANGE")` matches "EXCHANGE"; several corpus receipts
    // print a return policy, and reading one as change due would net a
    // real amount off the tender sum and invent a mismatch.
    let lines = vec![
        "TOTAL 20.00".to_string(),
        "CASH 20.00".to_string(),
        "No Refund, Exchange Only Within 7 Days 20.00".to_string(),
    ];
    assert_eq!(extract_change(&lines), 0);
    let tenders = extract_tenders(&lines);
    assert!(tenders_reconcile(&lines, &tenders, 2_000));
}

#[test]
fn cash_inside_a_longer_word_is_not_a_tender() {
    // `1424970 CASHMERE TP 26.99 H` is toilet paper in the item block, and
    // a bare `contains("CASH")` read it as a $26.99 cash payment — the only
    // tender on the receipt, so it warned that $40.83 was unaccounted for.
    // CASHIER lines are the same trap.
    let lines = vec![
        "1424970 CASHMERE TP 26.99 H".to_string(),
        "CASHIER: 12.00".to_string(),
        "**** TOTAL 67.82".to_string(),
    ];
    assert!(extract_tenders(&lines).is_empty());
}

#[test]
fn plain_cash_is_still_a_tender() {
    let lines = vec!["TOTAL 124.13".to_string(), "CASH 124.13".to_string()];
    let tenders = extract_tenders(&lines);
    assert_eq!(tenders.len(), 1);
    assert_eq!(tenders[0].kind, "cash");
}

#[test]
fn a_gift_card_balance_echo_is_not_a_second_tender() {
    // FreshCo prints the card's REMAINING balance after the purchase. It
    // carries the "GIFT CARD" keyword and a price, so it classified as a
    // second gift-card tender and every FreshCo gift-card receipt warned.
    // Guarding on BALANCE (not just Costco's "REMAINING BALANCE") covers
    // both wordings; no real tender line in either corpus says BALANCE.
    let lines = vec![
        "TOTAL TOTAL TAX $135.46 $3.37".to_string(),
        "Corp Gift Card TENDER $135.46".to_string(),
        "Gift Card Balance: $116.24".to_string(),
    ];
    let tenders = extract_tenders(&lines);
    assert_eq!(tenders.len(), 1);
    assert_eq!(tenders[0].amount_cents, 13_546);
    assert!(tenders_reconcile(&lines, &tenders, 13_546));
}

#[test]
fn a_thousands_separator_with_a_misread_zero_is_not_a_price() {
    // "Win a $1,000 PC gift card" comes back as `$1,00o`. The comma repair
    // only guards against a following *digit*, so `o` let it through and
    // manufactured a $1.00 price out of survey marketing copy — which then
    // classified as a gift-card tender on No Frills and RCSS receipts.
    assert_eq!(
        normalize_decimal_spacing("Vin a $1,00o PC gift card or"),
        "Vin a $1,00o PC gift card or"
    );
    let lines = vec![
        "TOTAL 6.88".to_string(),
        "Account: MASTERCARD CAD$ 6.88".to_string(),
        "Vin a $1,00o PC gift card or".to_string(),
    ];
    let tenders = extract_tenders(&lines);
    assert_eq!(tenders.len(), 1);
    assert!(tenders_reconcile(&lines, &tenders, 688));
}

#[test]
fn a_real_comma_decimal_point_still_parses() {
    // The repair this guard sits inside must keep working: OCR reads a
    // price's decimal point as a comma often enough to be worth repairing.
    assert_eq!(normalize_decimal_spacing("BANANAS 0,99"), "BANANAS 0.99");
    assert_eq!(
        normalize_decimal_spacing("TOTAL $12,50 H"),
        "TOTAL $12.50 H"
    );
}

#[test]
fn change_larger_than_the_tenders_is_dropped() {
    // Redaction re-scanned costco_46668 with the amount column shifted one
    // row: `MasterCard` / `CHANGE 441.68` / `0.00`. The card tender loses
    // its amount AND the card charge reads as change, so netting gives
    // -416.68 and the warning claims 883.36 is unaccounted for. No receipt
    // hands back more than it took in, so the change term is the untrusted
    // one. Dropping it is not a repair — the sum still misses the total,
    // and now by 441.68, which is exactly the tender that went missing.
    let lines = vec![
        "****TOTAL 466.68".to_string(),
        "Shop Card 25.00".to_string(),
        "MasterCard".to_string(),
        "CHANGE 441.68".to_string(),
        "0.00".to_string(),
    ];
    let tenders = extract_tenders(&lines);
    assert_eq!(tenders.len(), 1);
    assert_eq!(extract_change(&lines), 44_168);
    assert_eq!(tendered_net_cents(&lines, &tenders), 2_500);
    assert!(!tenders_reconcile(&lines, &tenders, 46_668));
}
