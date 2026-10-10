//! Deterministic, disk-backed wire representation for explicitly flat Numbers and Pages sources.
//! The immutable source and wire archive have distinct raw receipts but identical V2 trees.
use crate::{
    PackageDownload,
    package_semantic::{
        PackageSemanticIdentity, package_archive_semantic_identity_versioned,
        package_flat_archive_semantic_identity_v2,
    },
    write_staging::{WRITE_STAGING_FILE_LIMIT, WriteStagingFile},
};
use cirrove_core::{CancellationToken, ProviderError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{FileExt, MetadataExt},
};

const CHUNK: usize = 64 * 1024;
const MAX_ENTRIES: usize = 10_000;
type Result<T> = std::result::Result<T, ProviderError>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireReceipt {
    pub(crate) version: u8,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) expected_root: String,
}
impl WireReceipt {
    pub(crate) fn validate(&self, root: &str) -> Result<()> {
        validate_root(root)?;
        if self.version != 1
            || self.expected_root != root
            || self.size == 0
            || self.size > WRITE_STAGING_FILE_LIMIT
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid());
        }
        Ok(())
    }
}
fn invalid() -> ProviderError {
    ProviderError::Protocol("invalid flat native wire archive")
}
fn check(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(ProviderError::Cancelled)
    } else {
        Ok(())
    }
}
fn validate_root(root: &str) -> Result<()> {
    if root.len() > 1024
        || !matches!(
            cirrove_core::upload::native_package_suffix(root),
            Some(".numbers" | ".pages")
        )
        || root.eq_ignore_ascii_case(".numbers")
        || root.eq_ignore_ascii_case(".pages")
        || root
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
    {
        return Err(invalid());
    }
    Ok(())
}

/// Run only in a blocking closure retaining both staging owners. The output must
/// be a distinct empty budget-owned file; no descriptor or entry bytes escape.
/// Call again from the original source for stream rederivation; validate and
/// compare the returned receipt with the recorded receipt before any HTTP.
pub(crate) fn flat_numbers_wire(
    source: &WriteStagingFile,
    original: &PackageDownload,
    expected_root: &str,
    semantic: &PackageSemanticIdentity,
    wire: &WriteStagingFile,
    cancel: &CancellationToken,
) -> Result<WireReceipt> {
    if cirrove_core::upload::native_package_suffix(expected_root) != Some(".numbers") {
        return Err(invalid());
    }
    flat_native_wire(source, original, expected_root, semantic, wire, cancel)
}
/// Explicit Pages entrance; never guesses a format from archive members.
pub(crate) fn flat_pages_wire(
    source: &WriteStagingFile,
    original: &PackageDownload,
    expected_root: &str,
    semantic: &PackageSemanticIdentity,
    wire: &WriteStagingFile,
    cancel: &CancellationToken,
) -> Result<WireReceipt> {
    if cirrove_core::upload::native_package_suffix(expected_root) != Some(".pages") {
        return Err(invalid());
    }
    flat_native_wire(source, original, expected_root, semantic, wire, cancel)
}
fn flat_native_wire(
    source: &WriteStagingFile,
    original: &PackageDownload,
    expected_root: &str,
    semantic: &PackageSemanticIdentity,
    wire: &WriteStagingFile,
    cancel: &CancellationToken,
) -> Result<WireReceipt> {
    check(cancel)?;
    validate_root(expected_root)?;
    let source_file = source.borrowed_file();
    let wire_file = wire.borrowed_file();
    let a = source_file.metadata().map_err(|_| invalid())?;
    let b = wire_file.metadata().map_err(|_| invalid())?;
    if !b.is_file() || b.len() != 0 || (a.dev() == b.dev() && a.ino() == b.ino()) {
        return Err(invalid());
    }
    let actual = package_flat_archive_semantic_identity_v2(source_file, original, cancel)?;
    if &actual != semantic {
        return Err(ProviderError::VersionChanged);
    }
    let mut archive = zip::ZipArchive::new(source_file).map_err(|_| invalid())?;
    if archive.len() >= MAX_ENTRIES {
        return Err(invalid());
    }
    // Source validation already checked every name/type/CRC, duplicate and size.
    // Sorting exact UTF-8 names makes wire bytes independent of source ZIP order.
    let mut entries = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        check(cancel)?;
        let entry = archive.by_index(i).map_err(|_| invalid())?;
        let name = format!("{expected_root}/{}", entry.name());
        if name.len() > 1024 {
            return Err(invalid());
        }
        entries.push((name, i, entry.is_dir()));
    }
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let sink = WireSink {
        file: wire,
        cancel,
        position: 0,
        extent: 0,
    };
    let mut zip = zip::ZipWriter::new(sink);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::DEFAULT)
        .large_file(false);
    let zip_error = || {
        if cancel.is_cancelled() {
            ProviderError::Cancelled
        } else {
            invalid()
        }
    };
    zip.add_directory(format!("{expected_root}/"), options)
        .map_err(|_| zip_error())?;
    let mut buffer = [0; CHUNK];
    for (name, index, directory) in entries {
        check(cancel)?;
        if directory {
            zip.add_directory(name, options).map_err(|_| zip_error())?;
        } else {
            zip.start_file(name, options).map_err(|_| zip_error())?;
            let mut entry = archive.by_index(index).map_err(|_| invalid())?;
            loop {
                check(cancel)?;
                let n = entry.read(&mut buffer).map_err(|_| invalid())?;
                if n == 0 {
                    break;
                }
                zip.write_all(&buffer[..n]).map_err(|_| zip_error())?;
            }
        }
    }
    let sink = zip.finish().map_err(|_| zip_error())?;
    check(cancel)?;
    wire_file
        .sync_all()
        .map_err(|_| ProviderError::Unavailable)?;
    if wire_file.metadata().map_err(|_| invalid())?.len() != sink.extent {
        return Err(invalid());
    }
    let mut hash = Sha256::new();
    let mut offset = 0;
    while offset < sink.extent {
        check(cancel)?;
        let count =
            usize::try_from((sink.extent - offset).min(CHUNK as u64)).map_err(|_| invalid())?;
        wire_file
            .read_exact_at(&mut buffer[..count], offset)
            .map_err(|_| invalid())?;
        hash.update(&buffer[..count]);
        offset += count as u64;
    }
    let receipt = WireReceipt {
        version: 1,
        size: sink.extent,
        sha256: hex::encode(hash.finalize()),
        expected_root: expected_root.into(),
    };
    receipt.validate(expected_root)?;
    let raw = PackageDownload {
        size: receipt.size,
        sha256: receipt.sha256.clone(),
    };
    if package_archive_semantic_identity_versioned(wire_file, &raw, expected_root, 2, cancel)?
        != actual
        || package_flat_archive_semantic_identity_v2(source_file, original, cancel)? != actual
    {
        return Err(ProviderError::VersionChanged);
    }
    check(cancel)?;
    Ok(receipt)
}

struct WireSink<'a> {
    file: &'a WriteStagingFile,
    cancel: &'a CancellationToken,
    position: u64,
    extent: u64,
}
impl Write for WireSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("wire staging cancelled"));
        }
        let count = bytes.len().min(CHUNK);
        let end = self
            .position
            .checked_add(count as u64)
            .filter(|end| *end <= WRITE_STAGING_FILE_LIMIT)
            .ok_or_else(|| io::Error::other("wire staging size limit"))?;
        self.file
            .write_chunk_at(self.position, &bytes[..count])
            .map_err(|_| io::Error::other("wire staging write refused"))?;
        self.position = end;
        self.extent = self.extent.max(end);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.cancel.is_cancelled() {
            Err(io::Error::other("wire staging cancelled"))
        } else {
            Ok(())
        }
    }
}
impl Seek for WireSink<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("wire staging cancelled"));
        }
        let position = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            SeekFrom::End(n) => i128::from(self.extent) + i128::from(n),
        };
        if !(0..=i128::from(WRITE_STAGING_FILE_LIMIT)).contains(&position) {
            return Err(io::Error::other("wire staging seek limit"));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::write_staging::WriteStagingBudget;
    use std::{io::Cursor, os::unix::fs::PermissionsExt, sync::Arc};

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in entries {
            if name.ends_with('/') {
                zip.add_directory(*name, options).unwrap();
            } else {
                zip.start_file(*name, options).unwrap();
                zip.write_all(bytes).unwrap();
            }
        }
        zip.finish().unwrap().into_inner()
    }
    // Insert the exact bounded redundant local ZIP64 size pair observed in flat
    // exports, keeping classic central/local sizes and CRC and fixing offsets.
    fn mirror(mut raw: Vec<u8>) -> Vec<u8> {
        let end = raw.len() - 22;
        let center = u32::from_le_bytes(raw[end + 16..end + 20].try_into().unwrap()) as usize;
        let count = u16::from_le_bytes(raw[end + 10..end + 12].try_into().unwrap()) as usize;
        let name = u16::from_le_bytes(raw[26..28].try_into().unwrap()) as usize;
        let size = u32::from_le_bytes(raw[22..26].try_into().unwrap());
        let compressed = u32::from_le_bytes(raw[18..22].try_into().unwrap());
        assert_eq!(u16::from_le_bytes(raw[28..30].try_into().unwrap()), 0);
        let mut extra = vec![1, 0, 16, 0];
        extra.extend(u64::from(size).to_le_bytes());
        extra.extend(u64::from(compressed).to_le_bytes());
        raw[28..30].copy_from_slice(&20u16.to_le_bytes());
        raw.splice(30 + name..30 + name, extra);
        let mut at = center + 20;
        for i in 0..count {
            if i != 0 {
                let offset = u32::from_le_bytes(raw[at + 42..at + 46].try_into().unwrap());
                raw[at + 42..at + 46].copy_from_slice(&(offset + 20).to_le_bytes());
            }
            at += 46
                + u16::from_le_bytes(raw[at + 28..at + 30].try_into().unwrap()) as usize
                + u16::from_le_bytes(raw[at + 30..at + 32].try_into().unwrap()) as usize
                + u16::from_le_bytes(raw[at + 32..at + 34].try_into().unwrap()) as usize;
        }
        raw[end + 20 + 16..end + 20 + 20]
            .copy_from_slice(&u32::try_from(center + 20).unwrap().to_le_bytes());
        raw
    }
    fn stage(bytes: &[u8], budget: &WriteStagingBudget) -> Arc<WriteStagingFile> {
        let raw = tempfile::tempfile().unwrap();
        raw.set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let file = budget.adopt(raw).unwrap();
        for (i, chunk) in bytes.chunks(CHUNK).enumerate() {
            file.write_chunk_at((i * CHUNK) as u64, chunk).unwrap();
        }
        file
    }
    fn receipt(bytes: &[u8]) -> PackageDownload {
        PackageDownload {
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
        }
    }
    fn raw(file: &WriteStagingFile) -> Vec<u8> {
        let mut bytes = vec![0; file.borrowed_file().metadata().unwrap().len() as usize];
        file.borrowed_file().read_exact_at(&mut bytes, 0).unwrap();
        bytes
    }

    #[test]
    fn flat_mirrored_wire_preserves_all_content_and_is_deterministic() {
        let original = mirror(archive(&[
            ("Index/Document.iwa", b"native"),
            ("Empty/", b""),
            ("previews/preview.png", b"preview"),
            ("Metadata/Info.plist", b"info"),
        ]));
        let reordered = archive(&[
            ("Metadata/Info.plist", b"info"),
            ("previews/preview.png", b"preview"),
            ("Empty/", b""),
            ("Index/Document.iwa", b"native"),
        ]);
        let budget = WriteStagingBudget::new();
        let source = stage(&original, &budget);
        let other = stage(&reordered, &budget);
        let wire = stage(&[], &budget);
        let second = stage(&[], &budget);
        let cancel = CancellationToken::new();
        let semantic = package_flat_archive_semantic_identity_v2(
            source.borrowed_file(),
            &receipt(&original),
            &cancel,
        )
        .unwrap();
        let first = flat_numbers_wire(
            &source,
            &receipt(&original),
            "Owned.numbers",
            &semantic,
            &wire,
            &cancel,
        )
        .unwrap();
        let next = flat_numbers_wire(
            &other,
            &receipt(&reordered),
            "Owned.numbers",
            &semantic,
            &second,
            &cancel,
        )
        .unwrap();
        assert_eq!(raw(&source), original);
        assert_eq!(raw(&other), reordered);
        assert_eq!(first, next);
        assert_eq!(raw(&wire), raw(&second));
        first.validate("Owned.numbers").unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(raw(&wire))).unwrap();
        assert_eq!(zip.len(), 5);
        for (name, value) in [
            ("Index/Document.iwa", b"native".as_slice()),
            ("Empty/", b""),
            ("previews/preview.png", b"preview"),
            ("Metadata/Info.plist", b"info"),
        ] {
            let mut entry = zip.by_name(&format!("Owned.numbers/{name}")).unwrap();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, value);
        }
    }

    #[test]
    fn flat_pages_mirrored_wire_preserves_all_content_and_is_deterministic() {
        let original = mirror(archive(&[
            ("Index/Document.iwa", b"native"),
            ("Empty/", b""),
            ("previews/preview.png", b"preview"),
            ("Metadata/Info.plist", b"info"),
        ]));
        let reordered = archive(&[
            ("Metadata/Info.plist", b"info"),
            ("previews/preview.png", b"preview"),
            ("Empty/", b""),
            ("Index/Document.iwa", b"native"),
        ]);
        let budget = WriteStagingBudget::new();
        let source = stage(&original, &budget);
        let other = stage(&reordered, &budget);
        let wire = stage(&[], &budget);
        let second = stage(&[], &budget);
        let cancel = CancellationToken::new();
        let semantic = package_flat_archive_semantic_identity_v2(
            source.borrowed_file(),
            &receipt(&original),
            &cancel,
        )
        .unwrap();
        let first = flat_pages_wire(
            &source,
            &receipt(&original),
            "Owned.pages",
            &semantic,
            &wire,
            &cancel,
        )
        .unwrap();
        let next = flat_pages_wire(
            &other,
            &receipt(&reordered),
            "Owned.pages",
            &semantic,
            &second,
            &cancel,
        )
        .unwrap();
        assert_eq!(raw(&source), original);
        assert_eq!(raw(&other), reordered);
        assert_eq!(first, next);
        assert_eq!(raw(&wire), raw(&second));
        first.validate("Owned.pages").unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(raw(&wire))).unwrap();
        assert_eq!(zip.len(), 5);
        for (name, value) in [
            ("Index/Document.iwa", b"native".as_slice()),
            ("Empty/", b""),
            ("previews/preview.png", b"preview"),
            ("Metadata/Info.plist", b"info"),
        ] {
            let mut entry = zip.by_name(&format!("Owned.pages/{name}")).unwrap();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, value);
        }
    }

    #[test]
    fn bad_crc_path_receipt_semantic_and_root_refuse_before_wire_write() {
        let valid = archive(&[("Index/a", b"content")]);
        let mut bad_crc = valid.clone();
        let at = 30 + u16::from_le_bytes(bad_crc[26..28].try_into().unwrap()) as usize;
        bad_crc[at] ^= 1;
        let bad_path = archive(&[("../escape", b"content")]);
        let budget = WriteStagingBudget::new();
        let good = stage(&valid, &budget);
        let cancel = CancellationToken::new();
        let semantic = package_flat_archive_semantic_identity_v2(
            good.borrowed_file(),
            &receipt(&valid),
            &cancel,
        )
        .unwrap();
        for bytes in [bad_crc, bad_path] {
            let source = stage(&bytes, &budget);
            let wire = stage(&[], &budget);
            assert!(
                flat_numbers_wire(
                    &source,
                    &receipt(&bytes),
                    "Owned.numbers",
                    &semantic,
                    &wire,
                    &cancel
                )
                .is_err()
            );
            assert_eq!(raw(&source), bytes);
            assert!(raw(&wire).is_empty());
        }
        for arm in 0..3 {
            let wire = stage(&[], &budget);
            let mut expected = semantic.clone();
            let mut original = receipt(&valid);
            let root = if arm == 0 {
                "../Owned.numbers"
            } else {
                "Owned.numbers"
            };
            if arm == 1 {
                expected.sha256 = "0".repeat(64);
            }
            if arm == 2 {
                original.sha256 = "0".repeat(64);
            }
            assert!(flat_numbers_wire(&good, &original, root, &expected, &wire, &cancel).is_err());
            assert!(raw(&wire).is_empty());
            assert_eq!(raw(&good), valid);
        }
    }

    #[test]
    fn cancellation_and_disk_bounds_refuse_without_unbounded_write() {
        let bytes = archive(&[("Index/a", b"content")]);
        let budget = WriteStagingBudget::new();
        let source = stage(&bytes, &budget);
        let wire = stage(&[], &budget);
        let cancel = CancellationToken::new();
        let semantic = package_flat_archive_semantic_identity_v2(
            source.borrowed_file(),
            &receipt(&bytes),
            &cancel,
        )
        .unwrap();
        cancel.cancel();
        assert!(matches!(
            flat_numbers_wire(
                &source,
                &receipt(&bytes),
                "Owned.numbers",
                &semantic,
                &wire,
                &cancel
            ),
            Err(ProviderError::Cancelled)
        ));
        assert!(raw(&wire).is_empty());
        assert_eq!(raw(&source), bytes);
        let cancel = CancellationToken::new();
        let mut sink = WireSink {
            file: &wire,
            cancel: &cancel,
            position: 0,
            extent: 0,
        };
        assert!(
            sink.seek(SeekFrom::Start(WRITE_STAGING_FILE_LIMIT + 1))
                .is_err()
        );
        sink.seek(SeekFrom::Start(WRITE_STAGING_FILE_LIMIT))
            .unwrap();
        assert!(sink.write(b"x").is_err());
        assert!(raw(&wire).is_empty());
        sink.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(sink.write(&vec![7; CHUNK + 1]).unwrap(), CHUNK);
        cancel.cancel();
        assert!(sink.write(b"x").is_err());
        assert_eq!(raw(&wire).len(), CHUNK);
    }

    #[test]
    fn source_archive_and_canonical_entry_budgets_refuse_before_output() {
        let bytes = archive(&[("Index/a", b"content")]);
        let budget = WriteStagingBudget::new();
        let cancel = CancellationToken::new();
        let good = stage(&bytes, &budget);
        let semantic = package_flat_archive_semantic_identity_v2(
            good.borrowed_file(),
            &receipt(&bytes),
            &cancel,
        )
        .unwrap();
        let oversized = stage(&[], &budget);
        let wire = stage(&[], &budget);
        // Sparse synthetic source: the raw-size bound is checked before allocation.
        oversized
            .borrowed_file()
            .set_len(WRITE_STAGING_FILE_LIMIT + 1)
            .unwrap();
        let original = PackageDownload {
            size: WRITE_STAGING_FILE_LIMIT + 1,
            sha256: "0".repeat(64),
        };
        assert!(
            flat_numbers_wire(
                &oversized,
                &original,
                "Owned.numbers",
                &semantic,
                &wire,
                &cancel
            )
            .is_err()
        );
        assert_eq!(
            oversized.borrowed_file().metadata().unwrap().len(),
            original.size
        );
        assert!(raw(&wire).is_empty());
        drop(oversized);
        // 10,000 explicit files imply one additional root in canonical V2.
        let names: Vec<_> = (0..MAX_ENTRIES).map(|i| format!("file-{i:05}")).collect();
        let entries: Vec<_> = names
            .iter()
            .map(|n| (n.as_str(), b"x".as_slice()))
            .collect();
        let many = archive(&entries);
        let source = stage(&many, &budget);
        assert!(
            flat_numbers_wire(
                &source,
                &receipt(&many),
                "Owned.numbers",
                &semantic,
                &wire,
                &cancel
            )
            .is_err()
        );
        assert_eq!(raw(&source), many);
        assert!(raw(&wire).is_empty());
        assert_eq!(raw(&good), bytes);
    }

    #[test]
    fn flat_wire_preserves_uppercase_destination_wrapper() {
        let bytes = archive(&[("Index/a", b"content"), ("preview.jpg", b"preview")]);
        let budget = WriteStagingBudget::new();
        let cancel = CancellationToken::new();
        let source = stage(&bytes, &budget);
        let wire = stage(&[], &budget);
        let semantic = package_flat_archive_semantic_identity_v2(
            source.borrowed_file(),
            &receipt(&bytes),
            &cancel,
        )
        .unwrap();
        let result = flat_numbers_wire(
            &source,
            &receipt(&bytes),
            "Owned.NUMBERS",
            &semantic,
            &wire,
            &cancel,
        );
        assert_eq!(raw(&source), bytes);
        let actual = result.expect("public admission accepts uppercase Numbers suffix");
        actual.validate("Owned.NUMBERS").unwrap();
        assert_eq!(actual.expected_root, "Owned.NUMBERS");
        let mut zip = zip::ZipArchive::new(Cursor::new(raw(&wire))).unwrap();
        assert!(zip.by_name("Owned.NUMBERS/Index/a").is_ok());
    }

    #[test]
    fn wire_receipt_strict_version_root_size_and_hash() {
        let good = WireReceipt {
            version: 1,
            size: 1,
            sha256: "a".repeat(64),
            expected_root: "Owned.numbers".into(),
        };
        good.validate("Owned.numbers").unwrap();
        for arm in 0..6 {
            let mut r = good.clone();
            match arm {
                0 => r.version = 2,
                1 => r.size = 0,
                2 => r.size = WRITE_STAGING_FILE_LIMIT + 1,
                3 => r.sha256 = "A".repeat(64),
                4 => r.sha256 = "a".repeat(63),
                _ => r.expected_root = "Other.numbers".into(),
            }
            assert!(r.validate("Owned.numbers").is_err());
        }
        let mut json = serde_json::to_value(&good).unwrap();
        json["extra"] = true.into();
        assert!(serde_json::from_value::<WireReceipt>(json.clone()).is_err());
        json.as_object_mut().unwrap().remove("extra");
        json.as_object_mut().unwrap().remove("version");
        assert!(serde_json::from_value::<WireReceipt>(json).is_err());
    }
    #[test]
    fn flat_pages_wire_wrong_format_refuses_before_writing_source_or_wire() {
        let bytes = mirror(archive(&[
            ("Index/Document.iwa", b"content"),
            ("Empty/", b""),
        ]));
        let budget = WriteStagingBudget::new();
        let source = stage(&bytes, &budget);
        let cancel = CancellationToken::new();
        let semantic = package_flat_archive_semantic_identity_v2(
            source.borrowed_file(),
            &receipt(&bytes),
            &cancel,
        )
        .unwrap();
        for root in ["Owned.numbers", "Owned.key", "../Owned.pages", ".pages"] {
            let wire = stage(&[], &budget);
            assert!(
                flat_pages_wire(&source, &receipt(&bytes), root, &semantic, &wire, &cancel)
                    .is_err()
            );
            assert_eq!(raw(&source), bytes);
            assert!(raw(&wire).is_empty());
        }
        let wire = stage(&[], &budget);
        flat_pages_wire(
            &source,
            &receipt(&bytes),
            "Owned.pages",
            &semantic,
            &wire,
            &cancel,
        )
        .unwrap();
        assert_eq!(raw(&source), bytes);
        assert!(!raw(&wire).is_empty());
    }
}
