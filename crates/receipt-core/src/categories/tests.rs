use super::{
    build_rule_layers, classify_item_key, classify_item_tags, list_item_categories, mask_brands,
    resolve_account_target, BuildClassifierConfig, BuildRuleEntry, TagNode,
};
use crate::rules::{default_category_accounts, default_parser_rule_layers};
use std::collections::HashMap;

/// Ported from desktop `tests/test_item_categories.py`: a large corpus of
/// `description -> account` expectations the public classifier must satisfy.
/// These are the real-receipt regressions (priority ordering, longer-keyword
/// tiebreaks, brand-prefix wins, OCR D/O/0 confusable noise) that the desktop
/// suite guards; here they run natively against `receipt-core`, no PyO3.
#[test]
fn public_classifier_categorizes_real_receipt_items() {
    let cases: &[(&str, &str)] = &[
        ("Asahi - Mitsuya Cider Can", "Expenses:Food:Grocery:Drink"),
        ("Apple Cider", "Expenses:Food:AlcoholicBeverage"),
        ("Oyster Mushroom (L)", "Expenses:Food:Grocery:Vegetable"),
        ("Star Aniseed", "Expenses:Food:Grocery:Seasoning"),
        ("Star Anise", "Expenses:Food:Grocery:Seasoning"),
        (
            "Yang , SK - Pork / Leek Du",
            "Expenses:Food:Grocery:Frozen:Dumpling",
        ),
        ("Pork / Leek", "Expenses:Food:Grocery:Meat"),
        // staple — corn starch is sold beside the flour, but the produce
        // rule's bare "CORN" claimed it until "CORN STARCH" outgrew it on
        // the keyword-length tiebreak.
        ("WN CORN STARCH", "Expenses:Food:Grocery:Staple"),
        ("Cornstarch 454g", "Expenses:Food:Grocery:Staple"),
        // fruit — receipts print the vowel-dropped abbreviation, which the
        // full "WATERMELON" keyword never matches, so the drinks rule's
        // bare "WATER" claimed it.
        ("WATERMLN SGRBABY MRJ", "Expenses:Food:Grocery:Fruit"),
        ("WTRMLN SEEDLESS", "Expenses:Food:Grocery:Fruit"),
        // ...without dragging actual water out of the drinks rule.
        ("SPRING WATER 24PK", "Expenses:Food:Grocery:Drink"),
        // dairy — Neilson TruTaste milk matched no rule at all and fell
        // through to FIXME. TRUTASTE carries the match because the brand
        // token is what OCR mangles ("NLSN" -> "WLSN"/"HLSN"), not this.
        ("NLSN TRUTASTE 2%", "Expenses:Food:Grocery:Dairy"),
        ("WLSN TRUTASTE 28", "Expenses:Food:Grocery:Dairy"),
        // dairy — Loblaw's house-brand eggs print as "NN MED WHT OVWRP":
        // no name, Medium, WHiTe, OVerWRaP. The word "egg" is nowhere on
        // the line, so the phrase itself has to carry the match. OCR reads
        // the V as a U about as often as not, and truncates the tail.
        ("NN MED WHT OVWRP MRJ", "Expenses:Food:Grocery:Dairy"),
        ("NN MED WHT OUWRP MRJ", "Expenses:Food:Grocery:Dairy"),
        ("IN MED WHT O OUWRP", "Expenses:Food:Grocery:Dairy"),
        // seasoning
        (
            "SAPORITO FOODS CORN OIL 2.84L",
            "Expenses:Food:Grocery:Seasoning",
        ),
        (
            "FLOWER PERICARPIURN ZANTHOXYLI",
            "Expenses:Food:Grocery:Seasoning",
        ),
        (
            "T&T SLICED RED CHILI PEPPER",
            "Expenses:Food:Grocery:Seasoning",
        ),
        ("Pork Lard", "Expenses:Food:Grocery:Seasoning"),
        (
            "D.M.D White Pepper Powder",
            "Expenses:Food:Grocery:Seasoning",
        ),
        ("YYH Chillies 80g", "Expenses:Food:Grocery:Seasoning"),
        (
            "HD - Superior Qyster Sauc (",
            "Expenses:Food:Grocery:Seasoning",
        ),
        (
            "Choripdong - Hot Pepper P",
            "Expenses:Food:Grocery:Seasoning",
        ),
        // Narrow aliases above must not reclassify the underlying foods.
        ("Fresh Oyster Meat", "Expenses:Food:Grocery:Seafood"),
        ("Hot Pepper Plant", "Expenses:Food:Grocery:Vegetable"),
        // alcoholic
        ("COORS LIGHT 6 PK HQ", "Expenses:Food:AlcoholicBeverage"),
        ("MEAGHERS TRIPLE SEC", "Expenses:Food:AlcoholicBeverage"),
        ("BABY DUCK", "Expenses:Food:AlcoholicBeverage"),
        ("FROZEN DUCK", "Expenses:Food:Grocery:Frozen"),
        (
            "BBY - Frozen Snakehead Fi",
            "Expenses:Food:Grocery:Seafood:Fish",
        ),
        // pet
        ("2130150 HUGG WIPE", "Expenses:Pet:Supply"),
        // tooth care
        ("SONICARE TOOTHBRUSH HEADS", "Expenses:PersonalCare:Tooth"),
        ("1474938 COLGATE PR", "Expenses:PersonalCare:Tooth"),
        ("1457015 GLIDE ADV", "Expenses:PersonalCare:Tooth"),
        // Sensodyne brand name, no generic "TOOTHPASTE" token on the line
        // (Costco 2026-07-29).
        ("5592654 PRONAMEL", "Expenses:PersonalCare:Tooth"),
        // gift card: denomination glued to the brand, matched mid-token
        ("399 DOORDASH2X50", "Expenses:Food:Restaurant:GiftCard"),
        // snacks — a boxed Panda cookie is packaged, not fresh bakery. The
        // generic "COOKIE" keyword outranked every brand on the
        // keyword-length tiebreak until this rule was given a priority
        // (Costco 2026-08-05).
        ("969786 PANDA COOKIE", "Expenses:Food:Grocery:Snacks"),
        // dairy (incl. milk/chocolate tiebreak + single-char noise)
        ("Natrel - 2% Partly Skimme", "Expenses:Food:Grocery:Dairy"),
        ("Milk Chocolate 1%", "Expenses:Food:Grocery:Dairy"),
        (
            "NEILSON JOYYA CHOCOLATE E MILK",
            "Expenses:Food:Grocery:Dairy",
        ),
        ("1355285 TOGO VAN 2KG", "Expenses:Food:Grocery:Dairy"),
        // juice vs fruit-flavour keyword
        (
            "Tropicana - Blackberry Bl",
            "Expenses:Food:Grocery:Drink:Juice",
        ),
        // drink (brand-prefix / truncation / soft-drink-over-fruit)
        ("Soft Drink Orange", "Expenses:Food:Grocery:Drink"),
        ("YQSL - Grapefruit Tea Dri", "Expenses:Food:Grocery:Drink"),
        ("LZY - Original Flavor Dri", "Expenses:Food:Grocery:Drink"),
        ("Wing Hing Sweet Soy Bever", "Expenses:Food:Grocery:Drink"),
        ("*Yuan Qi Sen Lin Iced Tea", "Expenses:Food:Grocery:Drink"),
        (
            "'Tropicana Daily C Tea Dr ×1",
            "Expenses:Food:Grocery:Drink",
        ),
        ("TY - Lemon Tea", "Expenses:Food:Grocery:Drink"),
        // coffee
        ("108934 RAINFOREST", "Expenses:Food:Grocery:Drink:Coffee"),
        ("599010 LAVAZZA 1KG", "Expenses:Food:Grocery:Drink:Coffee"),
        // snacks
        ("HLY - Fish Cracker Seawee", "Expenses:Food:Grocery:Snacks"),
        ("1968518 WHITE RABBIT", "Expenses:Food:Grocery:Snacks"),
        ("*Sandwich Biscuits(Matcha)", "Expenses:Food:Grocery:Snacks"),
        (
            "*Or:ion Double Choco Pie 12",
            "Expenses:Food:Grocery:Snacks",
        ),
        ("La Pian (Spicy Gluten Sli", "Expenses:Food:Grocery:Snacks"),
        // potato crackers, column-truncated and OCR-damaged (Foody Mart)
        ("WJXZ - Okashi Potato Crac", "Expenses:Food:Grocery:Snacks"),
        ("WJXZ - Okashi Potato Crae", "Expenses:Food:Grocery:Snacks"),
        // juice beats the fruit it is made from
        ("Mr. Kon - Peach Juice", "Expenses:Food:Grocery:Drink:Juice"),
        ("FRT MANGO JUICE", "Expenses:Food:Grocery:Drink:Juice"),
        // staple
        ("LHL - Malatang Slightly S", "Expenses:Food:Grocery:Staple"),
        ("NX - Dried Sweet Potato V", "Expenses:Food:Grocery:Staple"),
        // ...but the produce itself is untouched
        ("Sweet Potato", "Expenses:Food:Grocery:Vegetable"),
        // Jin Tian's truncated wonton / dumplings, with and without the dash
        (
            "Jin Tian - Jumbo Shrimp C",
            "Expenses:Food:Grocery:Frozen:Dumpling",
        ),
        (
            "Jin Tian Jumbo Shrimp D",
            "Expenses:Food:Grocery:Frozen:Dumpling",
        ),
        (
            "LSK - Fish Dunplings",
            "Expenses:Food:Grocery:Frozen:Dumpling",
        ),
        // ...but unbranded jumbo shrimp is still shrimp
        (
            "Jumbo Shrimp Deveined",
            "Expenses:Food:Grocery:Seafood:Shrimp",
        ),
        // seafood
        (
            "BQ - Frozen Raw Peeled Un",
            "Expenses:Food:Grocery:Seafood:Shrimp",
        ),
        // bakery
        ("BAKERY", "Expenses:Food:Grocery:Bakery"),
        ("Red Bean Pinapple Bun", "Expenses:Food:Grocery:Bakery"),
        // prepared meal
        ("Hot Food", "Expenses:Food:Grocery:PreparedMeal"),
        // meat (prefer over lard false positive)
        ("Pork Large Intestine", "Expenses:Food:Grocery:Meat"),
        // fruit header
        ("&& Fruit (FT)", "Expenses:Food:Grocery:Fruit"),
        // clothing
        ("1944033 CHAMP SHORT", "Expenses:Shopping:Clothing"),
        ("2946010 SKECHERSGLID", "Expenses:Shopping:Clothing"),
        ("3966510 FO TANK S", "Expenses:Shopping:Clothing"),
        // household supply (incl. LYSOL with D/O/0 OCR confusables)
        ("295619 KS BAGS 60", "Expenses:Home:HouseholdSupply"),
        ("1218587 SWIFFER DUST", "Expenses:Home:HouseholdSupply"),
        ("3458556 TIDE CQLDWTR", "Expenses:Home:HouseholdSupply"),
        ("1727590 CASCADE PLUS", "Expenses:Home:HouseholdSupply"),
        ("1185 BAKING SODA", "Expenses:Home:HouseholdSupply"),
        ("1796144 TOLIET 2PK", "Expenses:Home:HouseholdSupply"),
        // "Tissue" abbreviated on the slip, so the TISSUE keyword misses it
        // (FreshCo unknown-date_freshco_157_38).
        ("Bath Tiss Jmbo 202s", "Expenses:Home:HouseholdSupply"),
        ("LYSOL BATH P 059631882930", "Expenses:Home:HouseholdSupply"),
        ("LYS0L BATH P 059631882930", "Expenses:Home:HouseholdSupply"),
        ("LYSDL BATH P 059631882930", "Expenses:Home:HouseholdSupply"),
        // personal care
        ("443404 MARC ANTHONY", "Expenses:PersonalCare"),
    ];

    // Build the bundled layers once; classify to a key then resolve to an
    // account (the desktop `categorize_item` chain).
    let layers = default_parser_rule_layers();
    let mapping: HashMap<String, String> = layers.category_rules.account_mapping.clone();
    let categorize = |description: &str| -> Option<String> {
        let key = classify_item_key(description, &layers.category_rules, None)?;
        resolve_account_target(Some(&key), &mapping, None)
    };

    let mut failures = Vec::new();
    for (desc, expected) in cases {
        let got = categorize(desc);
        if got.as_deref() != Some(*expected) {
            failures.push(format!("{desc:?} => {got:?}, expected {expected:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "categorization drift:\n{}",
        failures.join("\n")
    );
}

#[test]
fn truncated_seafood_pork_dumpling_alias_requires_the_full_product() {
    let layers = default_parser_rule_layers();
    let key = |description: &str| classify_item_key(description, &layers.category_rules, None);
    for description in [
        "Jin Tian Seafood & Pork Dum",
        "Jin Tlan Seafood & Fork Dum",
        "Jin Tian Seafood & Fork Dum",
        "Jin Tlan Seafood & Pork Dum",
    ] {
        assert_eq!(
            key(description).as_deref(),
            Some("grocery/frozen/dumpling"),
            "{description}"
        );
    }
    for description in [
        "Jin Tian Seafood Balls",
        "Jin Tian Pork Balls",
        "Seafood & Pork",
        "Other Brand Seafood & Fork Dum",
        "Fork 4PC Set",
    ] {
        assert_ne!(
            key(description).as_deref(),
            Some("grocery/frozen/dumpling"),
            "{description}"
        );
    }
}

#[test]
fn truncated_wonton_alias_requires_brand_and_product_prefix() {
    let layers = default_parser_rule_layers();
    let key = |description: &str| classify_item_key(description, &layers.category_rules, None);
    assert_eq!(
        key("100% Deluxe - Jumbo Shrim").as_deref(),
        Some("grocery/frozen/dumpling")
    );
    assert_eq!(
        key("RAW JUMBO SHRIMP").as_deref(),
        Some("grocery/seafood/shrimp")
    );
    for description in [
        "100% Deluxe",
        "DELUXE JUMBO SHRIMP",
        "ANOTHER BRAND - JUMBO SHRIM",
        "100% Deluxe - Steamed Pork Bun",
    ] {
        assert_ne!(
            key(description).as_deref(),
            Some("grocery/frozen/dumpling"),
            "{description}"
        );
    }
}

#[test]
fn roar_drink_requires_its_printed_product_code() {
    let layers = default_parser_rule_layers();
    assert_eq!(
        classify_item_key("4704960 ROAR", &layers.category_rules, None).as_deref(),
        Some("grocery/drink")
    );
    for description in ["ROAR", "4704961 ROAR", "HEAR ME ROAR"] {
        assert_ne!(
            classify_item_key(description, &layers.category_rules, None).as_deref(),
            Some("grocery/drink"),
            "{description}"
        );
    }
}

/// Semantic classification (key + tags) from the bundled rules. Ported from
/// the `classify_item_semantic` cases in `test_item_categories.py`; tags are
/// checked as a superset to stay robust to match ordering.
#[test]
fn semantic_classification_yields_expected_key_and_tags() {
    let layers = default_parser_rule_layers();
    let key = |d: &str| classify_item_key(d, &layers.category_rules, None);
    let tags = |d: &str| classify_item_tags(d, &layers.category_rules);

    assert_eq!(key("347937 CHICKEN").as_deref(), Some("grocery/meat"));
    for t in ["grocery", "grocery/meat", "grocery/meat/chicken"] {
        assert!(
            tags("347937 CHICKEN").iter().any(|x| x == t),
            "CHICKEN missing tag {t}"
        );
    }

    // Two rules claim a boxed Panda cookie, and only one of them is wrong
    // in a way a keyword can fix. `panda_cookie_snack` outranks the bakery
    // rule's generic "COOKIE" for the ACCOUNT; the bakery *tag* stays,
    // because the cookie really is a bakery product. The narrowly scoped
    // subtraction rules for false ingredient names do not apply here.
    //
    // The staple tag is the one that was simply false: the staple rule's
    // "DANDAN" keyword fuzzy-matched "PANDA". It is `exact_only` now, so
    // dan dan noodles still classify and cookies no longer do.
    assert_eq!(
        key("969786 PANDA COOKIE").as_deref(),
        Some("grocery/snacks")
    );
    assert_eq!(
        tags("969786 PANDA COOKIE"),
        vec!["grocery", "grocery/bakery", "grocery/snacks"]
    );
    assert_eq!(key("DANDAN NOODLE").as_deref(), Some("grocery/staple"));

    // The other `exact_only` keyword, and the clearer case for the
    // mechanism: "GINGER" fuzzy-matched "FINGER", so a beef rib came back
    // tagged as seasoning — and the app shows the most specific tag, which
    // made a 60-dollar cut of beef read as a spice. Nothing was misread
    // here; GINGER and FINGER are simply one letter apart, which is a
    // neighbor the fuzzy stage should never have entertained.
    assert_eq!(
        key("Beef Rib Boneless Finger").as_deref(),
        Some("grocery/meat")
    );
    assert_eq!(
        tags("Beef Rib Boneless Finger"),
        vec!["grocery", "grocery/meat"]
    );
    // ...while ginger itself still classifies.
    assert_eq!(key("Ginger Root").as_deref(), Some("grocery/seasoning"));

    // Salt is a seasoning, not a staple: a FOODY MART scan filed
    // "Windsor - Table Salt" under Staple because "SALT" sat in the staple
    // rule's keyword list beside FLOUR and SUGAR.
    assert_eq!(
        key("Windsor - Table Salt").as_deref(),
        Some("grocery/seasoning")
    );
    assert_eq!(
        tags("Windsor - Table Salt"),
        vec!["grocery", "grocery/seasoning"]
    );

    assert_eq!(key("435259 FINE-FILT").as_deref(), Some("grocery/dairy"));
    for t in ["grocery", "grocery/dairy", "grocery/dairy/milk"] {
        assert!(
            tags("435259 FINE-FILT").iter().any(|x| x == t),
            "FINE-FILT missing tag {t}"
        );
    }

    // LCBO card: tags only, no spendable category.
    assert_eq!(key("810 LCBO CARD"), None);
    for t in ["alcohol", "gift_card"] {
        assert!(
            tags("810 LCBO CARD").iter().any(|x| x == t),
            "LCBO missing tag {t}"
        );
    }

    // Amazon card off a Dollarama gift-card mall: same shape as LCBO CARD.
    assert_eq!(key("AMAZON.CA $25 07675062289"), None);
    assert!(
        tags("AMAZON.CA $25 07675062289")
            .iter()
            .any(|x| x == "gift_card"),
        "AMAZON missing tag gift_card"
    );
    // Its variable-denomination sibling off the same mall.
    assert_eq!(key("PNGO 25-500CAD 07675067936"), None);
    assert!(
        tags("PNGO 25-500CAD 07675067936")
            .iter()
            .any(|x| x == "gift_card"),
        "PNGO missing tag gift_card"
    );

    // FreshCo's "Ice Cream Strawberry": the flavour's STRAWBERRY outranked
    // ICE CREAM on length and filed it as Fruit, and FACE CREAM fuzzy-hit
    // "ICE CREAM" to hang Personal Care -- the label the app shows -- on it.
    assert_eq!(
        key("Ice Cream Strawberry").as_deref(),
        Some("grocery/frozen/icecream")
    );
    assert!(!tags("Ice Cream Strawberry")
        .iter()
        .any(|x| x.starts_with("personal_care")));
    assert_eq!(
        tags("Ice Cream Strawberry").last().map(String::as_str),
        Some("grocery/frozen/icecream")
    );
    assert_eq!(key("Nivea Face Cream").as_deref(), Some("personal_care"));
    // ...and the cooler whose flavour contains ICE CREAM stays alcohol.
    assert_eq!(
        key("SMIRNOFF ICE CREAMSICLE BLEST").as_deref(),
        Some("alcohol/beverage")
    );
    for glass in [
        "1-pc Wine Glass",
        "l-pc Wine Glass",
        "Martini/Cocktail Glass 19",
    ] {
        assert_eq!(key(glass).as_deref(), Some("household/supply"));
    }
    assert_eq!(key("RED WINE").as_deref(), Some("alcohol/beverage"));
    for name in ["[S] Rice bowl 115x63mm", "[S] 40 Tayo bowl 140x67mm"] {
        assert_eq!(key(name).as_deref(), Some("household/supply"), "{name}");
    }
    for food in ["RICE", "CHICKEN RICE BOWL"] {
        assert_ne!(key(food).as_deref(), Some("household/supply"), "{food}");
    }
    assert_eq!(
        key("Kobayashi Kantan Senjomar").as_deref(),
        Some("household/supply")
    );
    // FreshCo prints diced tomatoes as "Tom Diced".
    assert_eq!(
        key("Tom Diced").as_deref(),
        Some("grocery/vegetable/canned")
    );

    // Foody Mart truncates Sunrise's "Fresh Smooth Tofu" before TOFU; the
    // fuzzy SMOOTHIE used to claim it for juice. A real smoothie still is.
    assert_eq!(
        key("Sunrise - Fresh Smooth To").as_deref(),
        Some("grocery/vegetable")
    );
    assert_eq!(
        key("Oasis Smoothie Tropical M").as_deref(),
        Some("grocery/drink/juice")
    );

    // Asia Food Mart. OCR bridges the C of BISCUIT into an O; the flavour
    // words BEEF and SEAFOOD used to claim a vegetarian snack for Meat and
    // a rice noodle for Seafood.
    assert_eq!(
        key("Bisouit Orlginal Flavor").as_deref(),
        Some("grocery/snacks")
    );
    assert_eq!(
        key("ZhenXiang Beef Flavor Bea").as_deref(),
        Some("grocery/snacks")
    );
    assert_eq!(
        key("*Chawy Shiso & Seafood Fla").as_deref(),
        Some("grocery/staple")
    );
    // The same brand at Bestco, where BEAN in SOYBEAN had it as Staple.
    assert_eq!(
        key("Zhen Xiang Soybean Vegeta").as_deref(),
        Some("grocery/snacks")
    );
    // C/O lets "Pctato" read as POTATO; the noodle must stay a staple, and
    // a coupon line's "STORE COUPONS" must not read as OREO.
    assert_eq!(
        key("Sweet Pctato Noodle $").as_deref(),
        Some("grocery/staple")
    );
    assert_eq!(
        key("Sweet Potato Noodle").as_deref(),
        Some("grocery/staple")
    );
    assert_eq!(key("Kanufacturer/Store Compons HER CPN"), None);

    assert_eq!(
        key("2773717 MONSTER VRTY").as_deref(),
        Some("grocery/drink")
    );
    assert!(tags("2773717 MONSTER VRTY")
        .iter()
        .any(|x| x == "grocery/drink/energy_drink"));

    // FreshCo drops vowels ("Mnstr", "Wtr"), so the spelled-out `MONSTER` and
    // `COCONUT WATER` keywords never fire and a fuzzy hit fills the vacuum:
    // `RADISH` scores exactly 0.80 against a window of "Pa-RADIS-e", meeting
    // the 0.80 medium-keyword bar, and "Coconut Wtr" falls through to bare
    // `COCONUT` as fruit. The abbreviations are exact hits, and exactness
    // outranks fuzz at equal priority.
    assert_eq!(
        key("Mnstr Ultra Paradise").as_deref(),
        Some("grocery/drink")
    );
    assert!(tags("Mnstr Ultra Paradise")
        .iter()
        .any(|x| x == "grocery/drink/energy_drink"));
    assert_eq!(key("Cocomax Coconut Wtr").as_deref(), Some("grocery/drink"));

    // MARUTAI is a project-only override; public rules must not classify it.
    assert_eq!(key("MARUTAI"), None);
    assert!(tags("MARUTAI").is_empty());
}

/// Collisions that are decided by `keyword_length` — the THIRD key in
/// [`compare_match_rank`], below `priority` and `is_exact`.
///
/// These deserve a test of their own because the mechanism is invisible in
/// the rule file: nothing in `default_item_classifier.toml` states that
/// "SUNVINE" must outrank "SUGAR", it merely happens to be two characters
/// longer. Adding a keyword elsewhere in the file can flip any of these
/// silently, and the only other thing that would notice is a private-corpus
/// fixture the public gate cannot see.
///
/// `priority` cannot express these. It is a property of the *rule*, not the
/// keyword, so lifting the fruit rule above the staple rule to win one line
/// would also lift all ~45 of its other keywords — making "APPLE" beat the
/// juice rule's "APPLE JUICE", and "MELON" beat "WINTER MELON". Priority
/// also outranks exactness, so a bump lets fuzzy hits beat literal ones
/// (see the `NOODLES`/snacks note on `fuzzy_contains`). Length sits below
/// exactness and can never do that, which is why it stays the default and
/// these assertions exist instead.
///
/// Cases are from Foody Mart 2026-08-15 (`2026-08-15_foody_mart_47_48`),
/// whose ~25-character description truncation is what strands the product
/// word and leaves these collisions to be resolved by length at all.
#[test]
fn length_decided_keyword_collisions_stay_decided() {
    let layers = default_parser_rule_layers();
    let key = |d: &str| classify_item_key(d, &layers.category_rules, None);

    // "Sugar Baby" is a watermelon cultivar and "Sunvine" a watermelon
    // brand. The line truncates "Watermelon" to "Wate", so every watermelon
    // keyword misses and the staple rule's "SUGAR" is the only other hit —
    // it filed a melon as a staple. Both new keywords are longer than
    // "SUGAR" (10 and 7 vs 5); either alone would carry it, which is the
    // point of having both.
    assert_eq!(
        key("Sunvine - Sugar Baby Wate").as_deref(),
        Some("grocery/fruit")
    );
    // Guard the mechanism, not just the outcome: plain sugar is still a
    // staple, so this is a genuine collision and not the fruit rule
    // swallowing the keyword.
    assert_eq!(key("Rogers Sugar 2kg").as_deref(), Some("grocery/staple"));

    // "SCALLOP" (7) must outrank legacy_0001's "MEAT" (4) — the line
    // literally reads "Bay Scallop Meat". It must also land on plain
    // seafood rather than the narrow shrimp leaf it used to sit in.
    assert_eq!(
        key("BQ - Bay Scallop Meat 60-").as_deref(),
        Some("grocery/seafood")
    );
    // The sibling shellfish were deliberately left on the shrimp rule when
    // scallop moved off it. This asserts that scope decision, so whoever
    // finishes the job sees this line fail and updates it on purpose.
    assert_eq!(key("Squid Tent").as_deref(), Some("grocery/seafood/shrimp"));

    // Department-name line vs. the adjective inside a product name.
    // "VEGETABLE OIL" (13) must keep outranking "VEGETABLE" (9), or adding
    // the department word would have dragged every bottle of oil out of
    // seasoning.
    assert_eq!(key("Vegetables").as_deref(), Some("grocery/vegetable"));
    assert_eq!(
        key("Unico - Vegetable Oil").as_deref(),
        Some("grocery/seasoning")
    );

    // Not a length collision — a brand keyword standing in for a product
    // word that the truncation removed entirely. Included because it is the
    // same defect class: 紅薯粉絲 (sweet potato vermicelli) survives only on
    // the Chinese sub-line, which the OCR does not reliably detect, so
    // "VERMICELLI" has nothing to match and the line classified as nothing.
    assert_eq!(
        key("Shodoshima - Asian Style").as_deref(),
        Some("grocery/staple")
    );
}

/// The mask removes exactly the brand's own span, leaving the product text
/// — and its word boundaries — intact for the keyword stage.
#[test]
fn mask_brands_blanks_the_brand_span_only() {
    let brands = vec!["FISH WELL".to_string()];
    assert_eq!(
        mask_brands("Fish Well - Preserved Veg", &brands),
        "          - Preserved Veg"
    );
    // Whitespace on either side is ignored, because OCR splits and joins
    // words freely.
    assert_eq!(mask_brands("FishWell Sauce", &brands), "         Sauce");
    assert_eq!(mask_brands("Fish  Well Sauce", &brands), "           Sauce");
    // Every occurrence goes, not just the first.
    assert_eq!(
        mask_brands("Fish Well Fish Well", &brands),
        "                   "
    );
}

/// Short brands are the dangerous ones, so a brand only matches on word
/// boundaries — otherwise "LA" would eat the middle of "SALAD".
#[test]
fn mask_brands_requires_word_boundaries() {
    let brands = vec!["LA".to_string()];
    assert_eq!(mask_brands("Fresh Salad Mix", &brands), "Fresh Salad Mix");
    assert_eq!(mask_brands("LA - Rice Stick", &brands), "   - Rice Stick");
}

/// The two defects the brand table was introduced for: a keyword matching
/// inside a brand name and claiming the whole line.
#[test]
fn brands_stop_keywords_matching_inside_the_maker_name() {
    let layers = default_parser_rule_layers();
    let key = |d: &str| classify_item_key(d, &layers.category_rules, None);

    // FISH (legacy_0002) matches "Fish Well"; PRESERVED VEG must win the line.
    assert_eq!(
        key("Fish Well - Preserved Veg").as_deref(),
        Some("grocery/seasoning")
    );
    // CORN matches inside "Meat Corner"; BEEF must win the line.
    assert_eq!(
        key("Meat Corner - AA Beef Pla").as_deref(),
        Some("grocery/meat")
    );
}

/// `list_item_categories` returns path-sorted (path, account) pairs drawn
/// from both the account map and the rules. Mirrors the desktop test.
#[test]
fn list_item_categories_returns_sorted_key_account_pairs() {
    let config = BuildClassifierConfig {
        exact_only_keywords: vec![],
        brands: vec![],
        rules: vec![BuildRuleEntry {
            id: None,
            keywords: vec!["CUSTOM DIRECT ACCOUNT".to_string()],
            tag_paths: vec!["project/custom".to_string()],
            priority: 0,
            exact_only: false,
            ..Default::default()
        }],
    };
    let mut accounts = default_category_accounts();
    accounts.insert(
        "project/custom".to_string(),
        "Expenses:Project:Custom".to_string(),
    );
    accounts.insert("zzz_custom".to_string(), "Expenses:Project:Zzz".to_string());
    let layers = build_rule_layers(
        accounts,
        vec![config],
        vec![],
        vec![TagNode {
            path: "project/custom".to_string(),
            display: "Custom".to_string(),
        }],
    );

    let categories = list_item_categories(&layers);
    let mut sorted = categories.clone();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(categories, sorted, "not sorted by path");
    assert!(categories.contains(&(
        "project/custom".to_string(),
        "Expenses:Project:Custom".to_string()
    )));
    assert!(categories.contains(&("zzz_custom".to_string(), "Expenses:Project:Zzz".to_string())));
}

/// A project rule reaches an account by declaring a tag path that the
/// project's own `[accounts]` table maps (mirrors
/// `test_project_rule_key_maps_via_account_config`).
#[test]
fn project_rule_key_maps_via_account_config() {
    let config = BuildClassifierConfig {
        exact_only_keywords: vec![],
        brands: vec![],
        rules: vec![BuildRuleEntry {
            id: None,
            keywords: vec!["CUSTOM NOODLE BRAND".to_string()],
            tag_paths: vec!["grocery/staple".to_string()],
            priority: 20,
            exact_only: true,
            ..Default::default()
        }],
    };
    let accounts: HashMap<String, String> = [(
        "grocery/staple".to_string(),
        "Expenses:Food:Grocery:Staple".to_string(),
    )]
    .into_iter()
    .collect();
    let layers = build_rule_layers(
        accounts,
        vec![config],
        vec![],
        vec![TagNode {
            path: "grocery/staple".to_string(),
            display: "Staple".to_string(),
        }],
    );
    let key = classify_item_key("CUSTOM NOODLE BRAND", &layers, None).expect("classifies");
    let mapping: HashMap<String, String> = layers
        .account_mapping
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    assert_eq!(
        resolve_account_target(Some(&key), &mapping, None).as_deref(),
        Some("Expenses:Food:Grocery:Staple")
    );
}
