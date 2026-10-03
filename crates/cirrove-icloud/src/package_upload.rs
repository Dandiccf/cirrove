//! Pure parsing for the developer-only native package import validator.
//!
//! Apple Drive Build2636Build19 returns package asset receipts after uploading a
//! complete ZIP. This module only validates and reshapes that response; it does
//! not generate manifests, send requests, authorize writes, or confirm a document.
//! See docs/benchmarks/icloud-public-package-write-contract-2026-10-01.md.
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
};
use secrecy::SecretString;
use serde::{Deserialize, Deserializer, Serialize};

/// Local validator limits, not claimed Apple protocol maxima.
const MAX_RECEIPT_BYTES: usize = 64 * 1024;
const MAX_SECTIONS: usize = 64;
const MAX_HEX_BYTES: usize = 256;
const MAX_TOKEN_BYTES: usize = 4096;
const MAX_ASSET_BYTES: u64 = i64::MAX as u64;

/// Deliberately carries no provider body, token, key, or rejected input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageReceiptError;
impl std::fmt::Display for PackageReceiptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid or unsupported iCloud package upload receipt")
    }
}
impl std::error::Error for PackageReceiptError {}
type Result<T> = std::result::Result<T, PackageReceiptError>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackageReceipt {
    #[serde(rename = "hexBrSyntheticChecksum")]
    synthetic_checksum: String,
    #[serde(rename = "brManifest", default, deserialize_with = "present_asset")]
    br_manifest: Option<RawAsset>,
    #[serde(rename = "ckManifest", default, deserialize_with = "present_asset")]
    ck_manifest: Option<RawAsset>,
    #[serde(rename = "ckSectionAssets")]
    sections: Vec<RawAsset>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAsset {
    #[serde(rename = "hexFileChecksum")]
    checksum: String,
    #[serde(rename = "hexReferenceChecksum")]
    reference_checksum: String,
    #[serde(rename = "hexWrappingKey")]
    wrapping_key: String,
    #[serde(rename = "receiptToken")]
    receipt: String,
    size: u64,
}
fn present_asset<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<RawAsset>, D::Error> {
    // The inspected client schema uses optional(), not nullable(); this
    // validator rejects explicit null. Actual server responses remain unverified.
    RawAsset::deserialize(deserializer).map(Some)
}

// Never derive Debug: these objects contain provider wrapping keys and receipts.
#[derive(Serialize)]
struct Asset {
    signature: String,
    reference_signature: String,
    wrapping_key: String,
    receipt: String,
    size: u64,
}

/// A syntactically validated package response. It does not establish account,
/// operation, archive-content or remote-document identity; bind those separately
/// in a durable validator checkpoint before any future network operation.
pub struct PackageUploadReceipt {
    signature: String,
    br_manifest: Option<Asset>,
    ck_manifest: Option<Asset>,
    sections: Vec<Asset>,
}

/// Parse an expected Package response. Ordinary singleFile and mixed responses
/// are rejected, rather than silently changing the requested representation.
pub fn parse_package_upload_receipt(bytes: &[u8]) -> Result<PackageUploadReceipt> {
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(PackageReceiptError);
    }
    let raw: RawPackageReceipt = serde_json::from_slice(bytes).map_err(|_| PackageReceiptError)?;
    if raw.sections.len() > MAX_SECTIONS
        || (raw.br_manifest.is_none() && raw.ck_manifest.is_none() && raw.sections.is_empty())
    {
        return Err(PackageReceiptError);
    }
    let signature = hex_base64(&raw.synthetic_checksum)?;
    let mut total = 0u64;
    let br_manifest = raw
        .br_manifest
        .map(|asset| validate_asset(asset, &mut total))
        .transpose()?;
    let ck_manifest = raw
        .ck_manifest
        .map(|asset| validate_asset(asset, &mut total))
        .transpose()?;
    let sections = raw
        .sections
        .into_iter()
        .map(|asset| validate_asset(asset, &mut total))
        .collect::<Result<Vec<_>>>()?;
    let receipt = PackageUploadReceipt {
        signature,
        br_manifest,
        ck_manifest,
        sections,
    };
    // Validate the serialized fragment budget now, not only when later requested.
    let _ = receipt.registration_data()?;
    Ok(receipt)
}

fn hex_base64(value: &str) -> Result<String> {
    if value.is_empty() || value.len() > MAX_HEX_BYTES * 2 || !value.len().is_multiple_of(2) {
        return Err(PackageReceiptError);
    }
    let bytes = hex::decode(value).map_err(|_| PackageReceiptError)?;
    Ok(STANDARD.encode(bytes))
}
fn normalize_receipt(value: &str) -> Result<String> {
    if value.is_empty() || value.len() > MAX_TOKEN_BYTES {
        return Err(PackageReceiptError);
    }
    let standard = value.contains(['+', '/']);
    let url = value.contains(['-', '_']);
    if standard && url {
        return Err(PackageReceiptError);
    }
    let engine = match (url, value.contains('=')) {
        (false, true) => &STANDARD,
        (false, false) => &STANDARD_NO_PAD,
        (true, true) => &URL_SAFE,
        (true, false) => &URL_SAFE_NO_PAD,
    };
    let bytes = engine.decode(value).map_err(|_| PackageReceiptError)?;
    if bytes.is_empty() {
        return Err(PackageReceiptError);
    }
    Ok(STANDARD.encode(bytes))
}
fn validate_asset(raw: RawAsset, total: &mut u64) -> Result<Asset> {
    if raw.size > MAX_ASSET_BYTES {
        return Err(PackageReceiptError);
    }
    *total = total
        .checked_add(raw.size)
        .filter(|sum| *sum <= MAX_ASSET_BYTES)
        .ok_or(PackageReceiptError)?;
    Ok(Asset {
        signature: hex_base64(&raw.checksum)?,
        reference_signature: hex_base64(&raw.reference_checksum)?,
        wrapping_key: hex_base64(&raw.wrapping_key)?,
        receipt: normalize_receipt(&raw.receipt)?,
        size: raw.size,
    })
}
impl PackageUploadReceipt {
    /// Package-specific registration fields only, wrapped as a secret so a
    /// caller cannot accidentally print receipts via Debug. Does not include
    /// command, destination, owner, timestamps, or Apple's allow_conflict policy.
    /// Those require a separately validated and journal-bound creation context.
    pub fn registration_data(&self) -> Result<SecretString> {
        #[derive(Serialize)]
        struct Package<'a> {
            sections: &'a [Asset],
            manifest: &'a Option<Asset>,
        }
        #[derive(Serialize)]
        struct Registration<'a> {
            #[serde(rename = "pkgSignature")]
            signature: &'a str,
            manifest: &'a Option<Asset>,
            package: Package<'a>,
        }
        let data = Registration {
            signature: &self.signature,
            manifest: &self.br_manifest,
            package: Package {
                sections: &self.sections,
                manifest: &self.ck_manifest,
            },
        };
        let encoded = serde_json::to_string(&data).map_err(|_| PackageReceiptError)?;
        if encoded.len() > MAX_RECEIPT_BYTES {
            return Err(PackageReceiptError);
        }
        Ok(SecretString::from(encoded))
    }
}

#[cfg(test)]
mod tests;
