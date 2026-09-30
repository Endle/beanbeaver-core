//! Encoded-image boundary regression. Run with the models in `../../models`:
//! `cargo test --release -p bb-receipt-ffi --test orientation -- --ignored`

use bb_receipt_ffi::{DateYmd, OcrSession};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageEncoder, ImageFormat};
use sha2::{Digest, Sha256};
use std::io::Cursor;

#[test]
#[ignore = "needs converted models + public receipt fixture"]
fn scan_applies_exif_before_ocr_and_hashes_original_bytes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let session = OcrSession::new(root.join("../../models").display().to_string(), true)
        .expect("load models");
    let upright =
        image::open(root.join("../receipt-core/tests/receipts_e2e/costco_20260218_redact.jpg"))
            .unwrap()
            .to_rgb8();

    // Store pixels sideways, with EXIF orientation 6 (rotate 90 degrees CW).
    // TIFF little-endian header, one SHORT Orientation entry, no next IFD.
    let exif = vec![
        b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
    ];
    let mut sideways = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut sideways, 95);
    encoder.set_exif_metadata(exif).unwrap();
    encoder
        .encode_image(&DynamicImage::ImageRgb8(image::imageops::rotate270(
            &upright,
        )))
        .unwrap();

    // Use exactly the same decoded JPEG pixels for the upright reference,
    // encoded losslessly so JPEG recompression cannot change OCR results.
    let reference = image::load_from_memory(&sideways).unwrap().rotate90();
    let mut png = Cursor::new(Vec::new());
    reference.write_to(&mut png, ImageFormat::Png).unwrap();
    let png = png.into_inner();
    let scan = |bytes| {
        session
            .scan(
                bytes,
                DateYmd {
                    year: 2026,
                    month: 2,
                    day: 18,
                },
                "Liabilities:CreditCard".into(),
                "CAD".into(),
                "Expenses:Tax:HST".into(),
            )
            .expect("scan receipt")
    };
    let expected = scan(png.clone());
    let actual = scan(sideways.clone());
    assert_eq!(expected.merchant, "COSTCO");
    assert_eq!(expected.total, "221.97");
    assert_eq!(actual.raw_text, expected.raw_text);
    assert_eq!(actual.merchant, expected.merchant);
    assert_eq!(actual.date, expected.date);
    assert_eq!(actual.total, expected.total);
    assert_eq!(actual.items.len(), expected.items.len());

    // Identity is derived from the received bytes, not the oriented pixels.
    let expected_hash = format!("{:x}", Sha256::digest(&png));
    let actual_hash = format!("{:x}", Sha256::digest(&sideways));
    assert_ne!(actual_hash, expected_hash);
    assert!(actual.beancount.contains(&actual_hash));
    assert_eq!(
        actual.beanbeaver_id,
        Some(format!("bb-20260218-{}", &actual_hash[..8]))
    );
    assert_eq!(
        actual
            .beancount
            .replace(&actual_hash, "IMAGE_HASH")
            .replace(&actual_hash[..8], "IMAGE_HASH"),
        expected
            .beancount
            .replace(&expected_hash, "IMAGE_HASH")
            .replace(&expected_hash[..8], "IMAGE_HASH"),
    );
}
