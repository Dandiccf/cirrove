use super::*;
use secrecy::ExposeSecret;
use serde_json::{Value, json};

fn asset() -> Value {
    json!({"hexFileChecksum":"00ff","hexReferenceChecksum":"0102",
        "hexWrappingKey":"0304","receiptToken":"_w","size":7})
}
fn receipt() -> Value {
    json!({"hexBrSyntheticChecksum":"000102fe","ckSectionAssets":[asset()]})
}
fn parse(value: &Value) -> Result<PackageUploadReceipt> {
    parse_package_upload_receipt(&serde_json::to_vec(value).expect("synthetic JSON"))
}
fn registration(value: &Value) -> Value {
    let parsed = parse(value).expect("valid synthetic package receipt");
    serde_json::from_str(
        parsed
            .registration_data()
            .expect("bounded fragment")
            .expose_secret(),
    )
    .expect("registration JSON")
}
fn mapped_asset() -> Value {
    json!({"signature":"AP8=","reference_signature":"AQI=",
        "wrapping_key":"AwQ=","receipt":"/w==","size":7})
}

#[test]
fn package_receipt_maps_distinct_manifests_and_preserves_section_order() {
    let mut input = receipt();
    let mut br = asset();
    br["hexFileChecksum"] = json!("10");
    let mut ck = asset();
    ck["hexFileChecksum"] = json!("20");
    let mut second = asset();
    second["hexFileChecksum"] = json!("30");
    input["brManifest"] = br;
    input["ckManifest"] = ck;
    input["ckSectionAssets"] = json!([asset(), second]);
    let mut expected_br = mapped_asset();
    expected_br["signature"] = json!("EA==");
    let mut expected_ck = mapped_asset();
    expected_ck["signature"] = json!("IA==");
    let mut expected_second = mapped_asset();
    expected_second["signature"] = json!("MA==");
    assert_eq!(
        registration(&input),
        json!({
            "pkgSignature":"AAEC/g==", "manifest":expected_br,
            "package":{"manifest":expected_ck,"sections":[mapped_asset(),expected_second]}
        })
    );
}

#[test]
fn package_receipt_optional_manifests_are_absent_not_null_or_malformed_inputs() {
    let output = registration(&receipt());
    assert_eq!(output["manifest"], Value::Null);
    assert_eq!(output["package"]["manifest"], Value::Null);
    for key in ["brManifest", "ckManifest"] {
        for value in [Value::Null, json!({}), json!("secret-invalid-manifest")] {
            let mut input = receipt();
            input[key] = value;
            assert!(parse(&input).is_err());
        }
    }
    let mut input = receipt();
    input["ckSectionAssets"] = json!([]);
    assert!(
        parse(&input).is_err(),
        "local policy requires at least one asset"
    );
    input["brManifest"] = asset();
    assert!(
        parse(&input).is_ok(),
        "manifest-only package shape is permitted"
    );
}

#[test]
fn package_receipt_rejects_ordinary_mixed_unknown_and_duplicate_fields() {
    assert!(parse(&json!({"singleFile":{}})).is_err());
    let mut mixed = receipt();
    mixed["singleFile"] = json!({"size":7});
    assert!(parse(&mixed).is_err());
    let mut unknown = receipt();
    unknown["unrecognizedRepresentation"] = json!(true);
    assert!(parse(&unknown).is_err());
    let mut nested = receipt();
    nested["ckSectionAssets"][0]["owner"] = json!("untrusted-owner");
    assert!(parse(&nested).is_err());
    let original = serde_json::to_string(&receipt()).expect("synthetic JSON");
    let duplicate = original.replacen('{', "{\"hexBrSyntheticChecksum\":\"ff\",", 1);
    assert!(parse_package_upload_receipt(duplicate.as_bytes()).is_err());
}

#[test]
fn package_receipt_hex_is_decoded_and_bounded_without_assuming_digest_algorithm() {
    let mut input = receipt();
    input["hexBrSyntheticChecksum"] = json!("aB");
    assert_eq!(registration(&input)["pkgSignature"], "qw==");
    for invalid in [
        "".to_owned(),
        "0".into(),
        "gg".into(),
        "é".into(),
        "00".repeat(MAX_HEX_BYTES + 1),
    ] {
        input["hexBrSyntheticChecksum"] = json!(invalid);
        assert!(parse(&input).is_err());
    }
    input["hexBrSyntheticChecksum"] = json!("00".repeat(MAX_HEX_BYTES));
    assert!(parse(&input).is_ok());
    for key in ["hexFileChecksum", "hexReferenceChecksum", "hexWrappingKey"] {
        let mut invalid = receipt();
        invalid["ckSectionAssets"][0][key] = json!("bad-hex");
        assert!(parse(&invalid).is_err());
    }
}

#[test]
fn package_receipt_base64_is_canonical_and_rejects_invalid_or_mixed_encoding() {
    for token in ["/w==", "_w==", "/w", "_w"] {
        let mut input = receipt();
        input["ckSectionAssets"][0]["receiptToken"] = json!(token);
        assert_eq!(
            registration(&input)["package"]["sections"][0]["receipt"],
            "/w=="
        );
    }
    for token in ["", " ", "Zg=", "Zg===", "Zg==\n", "+_AA", "AB", "é"] {
        let mut input = receipt();
        input["ckSectionAssets"][0]["receiptToken"] = json!(token);
        assert!(
            parse(&input).is_err(),
            "invalid synthetic encoding accepted"
        );
    }
    let mut input = receipt();
    input["ckSectionAssets"][0]["receiptToken"] = json!("A".repeat(MAX_TOKEN_BYTES));
    assert!(parse(&input).is_ok());
    input["ckSectionAssets"][0]["receiptToken"] = json!("A".repeat(MAX_TOKEN_BYTES + 1));
    assert!(parse(&input).is_err());
}

#[test]
fn package_receipt_byte_and_section_limits_apply_at_the_boundary() {
    let mut body = serde_json::to_vec(&receipt()).expect("synthetic JSON");
    body.resize(MAX_RECEIPT_BYTES, b' ');
    assert!(parse_package_upload_receipt(&body).is_ok());
    body.push(b' ');
    assert!(parse_package_upload_receipt(&body).is_err());
    let mut input = receipt();
    input["ckSectionAssets"] = Value::Array(vec![asset(); MAX_SECTIONS]);
    assert!(parse(&input).is_ok());
    input["ckSectionAssets"] = Value::Array(vec![asset(); MAX_SECTIONS + 1]);
    assert!(parse(&input).is_err());
}

#[test]
fn package_receipt_sizes_are_integral_and_total_is_bounded_including_manifests() {
    for size in [json!(-1), json!(1.5), json!("7"), json!(u64::MAX)] {
        let mut input = receipt();
        input["ckSectionAssets"][0]["size"] = size;
        assert!(parse(&input).is_err());
    }
    let mut input = receipt();
    input["ckSectionAssets"][0]["size"] = json!(MAX_ASSET_BYTES);
    assert!(parse(&input).is_ok());
    let mut manifest = asset();
    manifest["size"] = json!(1);
    input["brManifest"] = manifest;
    assert!(
        parse(&input).is_err(),
        "manifest bytes count toward aggregate bound"
    );
    input["ckSectionAssets"][0]["size"] = json!(0);
    assert!(
        parse(&input).is_ok(),
        "zero-sized assets do not imply absent receipts"
    );
}

#[test]
fn package_receipt_errors_and_registration_do_not_leak_or_invent_authority() {
    let marker = "PRIVATE-RECEIPT-MARKER!";
    let mut input = receipt();
    input["ckSectionAssets"][0]["receiptToken"] = json!(marker);
    let error = match parse(&input) {
        Err(error) => error,
        Ok(_) => panic!("invalid marker accepted"),
    };
    assert_eq!(
        error.to_string(),
        "invalid or unsupported iCloud package upload receipt"
    );
    assert!(!format!("{error:?}").contains(marker));
    let fragment = registration(&receipt());
    for key in [
        "command",
        "allow_conflict",
        "document_id",
        "path",
        "owner",
        "account",
        "operation",
        "mtime",
        "btime",
    ] {
        assert!(fragment.get(key).is_none());
    }
    let parsed = parse(&receipt()).expect("synthetic receipt");
    let secret = parsed.registration_data().expect("secret fragment");
    assert!(!format!("{secret:?}").contains("pkgSignature"));
}
