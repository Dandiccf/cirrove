//! Independent bounded ZIP-content comparison for the owned PACKAGE validator.
//! No extraction, rewriting, native-document parsing or provider calls.
use crate::PackageDownload;
use cirrove_core::{CancellationToken, ProviderError};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Cursor, Read},
    os::unix::fs::FileExt,
};
type Result<T> = std::result::Result<T, ProviderError>;
const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;
const MAX_EXPANDED: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_CENTRAL: usize = 16 * 1024 * 1024;
const MAX_NAME: usize = 1024;
fn invalid() -> ProviderError {
    ProviderError::Protocol("unsupported or invalid package archive")
}
fn check(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(ProviderError::Cancelled)
    } else {
        Ok(())
    }
}
fn u16_at(bytes: &[u8], at: usize) -> Result<usize> {
    let v = bytes.get(at..at + 2).ok_or_else(invalid)?;
    Ok(u16::from_le_bytes([v[0], v[1]]) as usize)
}
fn u32_at(bytes: &[u8], at: usize) -> Result<usize> {
    let v = bytes.get(at..at + 4).ok_or_else(invalid)?;
    Ok(u32::from_le_bytes([v[0], v[1], v[2], v[3]]) as usize)
}
#[derive(PartialEq, Eq)]
struct Entry {
    directory: bool,
    size: u64,
    sha256: String,
}
struct Fingerprint {
    entries: BTreeMap<String, Entry>,
    expanded: u64,
    files: usize,
}
/// Counts and archive digests only; never contains entry names or document text.
pub struct PackageSemanticComparison {
    pub entries: usize,
    pub files: usize,
    pub expanded_bytes: u64,
    pub source_archive_sha256: String,
    pub imported_archive_sha256: String,
    pub raw_archives_equal: bool,
}
pub use cirrove_core::upload::{PACKAGE_SEMANTIC_IDENTITY_VERSION, PackageSemanticIdentity};
const IDENTITY_DOMAIN: &[u8] = b"cirrove.package.semantic.identity\0";
/// Compute a stable, explicitly root-bound content identity from a bounded ZIP.
/// Verifies the independent raw receipt before parsing. Call from spawn_blocking.
/// Archive order, compression, timestamps and the expected wrapper name do not
/// affect the identity. Every inner path, kind, size and content digest does.
pub fn package_archive_semantic_identity(
    archive: &File,
    receipt: &PackageDownload,
    expected_root: &str,
    cancel: &CancellationToken,
) -> Result<PackageSemanticIdentity> {
    validate_root(expected_root)?;
    let content = bind_root(
        fingerprint(archive, receipt, cancel, MAX_EXPANDED)?,
        expected_root,
    )?;
    let sha256 = identity_digest(
        &content,
        IDENTITY_DOMAIN,
        PACKAGE_SEMANTIC_IDENTITY_VERSION,
        cancel,
    )?;
    Ok(PackageSemanticIdentity {
        version: PACKAGE_SEMANTIC_IDENTITY_VERSION,
        sha256,
        entries: u32::try_from(content.entries.len()).map_err(|_| invalid())?,
        files: u32::try_from(content.files).map_err(|_| invalid())?,
        expanded_bytes: content.expanded,
    })
}
fn identity_digest(
    content: &Fingerprint,
    domain: &[u8],
    version: u32,
    cancel: &CancellationToken,
) -> Result<String> {
    check(cancel)?;
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(version.to_be_bytes());
    hash.update(
        u32::try_from(content.entries.len())
            .map_err(|_| invalid())?
            .to_be_bytes(),
    );
    hash.update(
        u32::try_from(content.files)
            .map_err(|_| invalid())?
            .to_be_bytes(),
    );
    hash.update(content.expanded.to_be_bytes());
    // BTreeMap<String, _> provides exact UTF-8 lexical ordering, with no Unicode
    // normalization or case folding. Lengths frame all variable-sized fields.
    for (path, entry) in &content.entries {
        check(cancel)?;
        hash.update([if entry.directory { b'D' } else { b'F' }]);
        hash.update(
            u32::try_from(path.len())
                .map_err(|_| invalid())?
                .to_be_bytes(),
        );
        hash.update(path.as_bytes());
        hash.update(entry.size.to_be_bytes());
        let digest = hex::decode(&entry.sha256).map_err(|_| invalid())?;
        if digest.len() != 32 {
            return Err(invalid());
        }
        hash.update(digest);
    }
    Ok(hex::encode(hash.finalize()))
}

/// Compare exact entry paths, types, lengths and decompressed SHA-256. ZIP order,
/// compression choice and timestamps may differ. Call from a blocking task.
/// Private input bytes are snapshotted (at most 64 MiB at a time), removing file
/// mutation races between hashing and parsing; source receipts must be independent.
pub fn compare_package_archives(
    source: &File,
    source_receipt: &PackageDownload,
    imported: &File,
    imported_receipt: &PackageDownload,
    cancel: &CancellationToken,
) -> Result<PackageSemanticComparison> {
    let a = fingerprint(source, source_receipt, cancel, MAX_EXPANDED)?;
    let b = fingerprint(imported, imported_receipt, cancel, MAX_EXPANDED)?;
    compare_fingerprints(a, b, source_receipt, imported_receipt, cancel)
}

/// Compare beneath exactly the caller-bound enclosing package directory on each
/// side. Each expected root must be one safe path component. No root is inferred
/// from archive contents; no second prefix, inner path or directory is ignored.
/// Explicit root directory entries, when present, remain part of the comparison.
/// The ordinary strict comparison above deliberately retains its old contract.
pub fn compare_package_archives_with_roots(
    source: &File,
    source_receipt: &PackageDownload,
    expected_source_root: &str,
    imported: &File,
    imported_receipt: &PackageDownload,
    expected_imported_root: &str,
    cancel: &CancellationToken,
) -> Result<PackageSemanticComparison> {
    validate_root(expected_source_root)?;
    validate_root(expected_imported_root)?;
    let a = bind_root(
        fingerprint(source, source_receipt, cancel, MAX_EXPANDED)?,
        expected_source_root,
    )?;
    let b = bind_root(
        fingerprint(imported, imported_receipt, cancel, MAX_EXPANDED)?,
        expected_imported_root,
    )?;
    compare_fingerprints(a, b, source_receipt, imported_receipt, cancel)
}
fn validate_root(root: &str) -> Result<()> {
    if root.contains('/') || safe_name(root.as_bytes(), false)? != root {
        return Err(ProviderError::Protocol("invalid expected package root"));
    }
    Ok(())
}
fn bind_root(mut fingerprint: Fingerprint, root: &str) -> Result<Fingerprint> {
    let prefix = format!("{root}/");
    let mut entries = BTreeMap::new();
    if fingerprint.files == 0 {
        return Err(ProviderError::Protocol("package root has no file content"));
    }
    for (name, entry) in fingerprint.entries {
        let relative = if name == root {
            if !entry.directory {
                return Err(ProviderError::Protocol("package root is not a directory"));
            }
            ""
        } else {
            name.strip_prefix(&prefix)
                .filter(|name| !name.is_empty())
                .ok_or(ProviderError::Protocol(
                    "package entry lies outside expected root",
                ))?
        };
        // Existing fingerprint validation already rejected unsafe paths and
        // duplicates. Keep this insertion guard explicit at the new boundary.
        if entries.insert(relative.to_owned(), entry).is_some() {
            return Err(invalid());
        }
    }
    fingerprint.entries = entries;
    Ok(fingerprint)
}
fn compare_fingerprints(
    a: Fingerprint,
    b: Fingerprint,
    source_receipt: &PackageDownload,
    imported_receipt: &PackageDownload,
    cancel: &CancellationToken,
) -> Result<PackageSemanticComparison> {
    check(cancel)?;
    if a.entries != b.entries {
        return Err(ProviderError::Protocol("package contents differ"));
    }
    Ok(PackageSemanticComparison {
        entries: a.entries.len(),
        files: a.files,
        expanded_bytes: a.expanded,
        source_archive_sha256: source_receipt.sha256.clone(),
        imported_archive_sha256: imported_receipt.sha256.clone(),
        raw_archives_equal: source_receipt.size == imported_receipt.size
            && source_receipt.sha256 == imported_receipt.sha256,
    })
}
fn snapshot(file: &File, receipt: &PackageDownload, cancel: &CancellationToken) -> Result<Vec<u8>> {
    check(cancel)?;
    let meta = file.metadata().map_err(|_| invalid())?;
    if !meta.is_file()
        || receipt.size == 0
        || receipt.size > MAX_ARCHIVE
        || meta.len() != receipt.size
    {
        return Err(invalid());
    }
    let mut bytes = vec![0; receipt.size as usize];
    let mut hash = Sha256::new();
    for (i, chunk) in bytes.chunks_mut(64 * 1024).enumerate() {
        check(cancel)?;
        file.read_exact_at(chunk, (i * 64 * 1024) as u64)
            .map_err(|_| invalid())?;
        hash.update(chunk);
    }
    if hex::encode(hash.finalize()) != receipt.sha256 {
        return Err(ProviderError::VersionChanged);
    }
    Ok(bytes)
}
fn safe_name(raw: &[u8], directory: bool) -> Result<String> {
    if raw.is_empty() || raw.len() > MAX_NAME {
        return Err(invalid());
    }
    let name = std::str::from_utf8(raw).map_err(|_| invalid())?;
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '\\' | ':'))
        || name.starts_with('/')
    {
        return Err(invalid());
    }
    let key = if directory {
        name.strip_suffix('/').ok_or_else(invalid)?
    } else {
        name
    };
    if key
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid());
    }
    Ok(key.to_owned())
}
fn extras(bytes: &[u8]) -> Result<()> {
    let mut at = 0;
    while at < bytes.len() {
        let kind = u16_at(bytes, at)?;
        let len = u16_at(bytes, at + 2)?;
        at = at.checked_add(4 + len).ok_or_else(invalid)?;
        if kind == 1 || at > bytes.len() {
            return Err(invalid());
        }
    }
    Ok(())
}
// Validate every central entry before the ZIP library can coalesce duplicate
// names or allocate metadata. Classic, single-disk archives only; no ZIP64.
fn layout(bytes: &[u8], cancel: &CancellationToken, expanded_limit: u64) -> Result<usize> {
    if bytes.len() < 22 {
        return Err(invalid());
    }
    let lower = bytes.len().saturating_sub(65557);
    let end = (lower..=bytes.len() - 22)
        .rev()
        .find(|&at| {
            bytes[at..].starts_with(b"PK\x05\x06")
                && u16_at(bytes, at + 20).is_ok_and(|n| at + 22 + n == bytes.len())
        })
        .ok_or_else(invalid)?;
    let count = u16_at(bytes, end + 10)?;
    let size = u32_at(bytes, end + 12)?;
    let start = u32_at(bytes, end + 16)?;
    if u16_at(bytes, end + 4)? != 0
        || u16_at(bytes, end + 6)? != 0
        || u16_at(bytes, end + 8)? != count
        || count == 0
        || count > MAX_ENTRIES
        || size > MAX_CENTRAL
        || start.checked_add(size) != Some(end)
    {
        return Err(invalid());
    }
    let mut at = start;
    let mut names = BTreeMap::new();
    let mut ranges = Vec::with_capacity(count);
    let mut expanded = 0u64;
    for _ in 0..count {
        check(cancel)?;
        if bytes.get(at..at + 4) != Some(b"PK\x01\x02") {
            return Err(invalid());
        }
        let flags = u16_at(bytes, at + 8)?;
        let method = u16_at(bytes, at + 10)?;
        let compressed = u32_at(bytes, at + 20)?;
        let uncompressed = u32_at(bytes, at + 24)?;
        let name_len = u16_at(bytes, at + 28)?;
        let extra = u16_at(bytes, at + 30)?;
        let comment = u16_at(bytes, at + 32)?;
        let next = at
            .checked_add(46 + name_len + extra + comment)
            .ok_or_else(invalid)?;
        if next > end
            || name_len > MAX_NAME
            || flags & !0x080e != 0
            || ![0, 8].contains(&method)
            || u16_at(bytes, at + 34)? != 0
        {
            return Err(invalid());
        }
        let raw = bytes.get(at + 46..at + 46 + name_len).ok_or_else(invalid)?;
        let directory = raw.ends_with(b"/");
        let name = safe_name(raw, directory)?;
        if names.insert(name, directory).is_some() {
            return Err(invalid());
        }
        extras(&bytes[at + 46 + name_len..at + 46 + name_len + extra])?;
        let mode = u32_at(bytes, at + 38)? >> 16;
        let kind = mode & 0o170000;
        if ![0, 0o100000, 0o040000].contains(&kind)
            || (kind == 0o040000 && !directory)
            || (kind == 0o100000 && directory)
            || (directory && uncompressed != 0)
        {
            return Err(invalid());
        }
        expanded = expanded
            .checked_add(uncompressed as u64)
            .ok_or_else(invalid)?;
        if expanded > expanded_limit {
            return Err(invalid());
        }
        let local = u32_at(bytes, at + 42)?;
        if local >= start {
            return Err(invalid());
        }
        if bytes.get(local..local + 4) != Some(b"PK\x03\x04")
            || u16_at(bytes, local + 6)? != flags
            || u16_at(bytes, local + 8)? != method
            || u16_at(bytes, local + 26)? != name_len
        {
            return Err(invalid());
        }
        let local_extra = u16_at(bytes, local + 28)?;
        let data = local
            .checked_add(30 + name_len + local_extra)
            .ok_or_else(invalid)?;
        let finish = data.checked_add(compressed).ok_or_else(invalid)?;
        if finish > start || bytes.get(local + 30..local + 30 + name_len) != Some(raw) {
            return Err(invalid());
        }
        extras(bytes.get(local + 30 + name_len..data).ok_or_else(invalid)?)?;
        if flags & 8 == 0
            && (u32_at(bytes, local + 14)? != u32_at(bytes, at + 16)?
                || u32_at(bytes, local + 18)? != compressed
                || u32_at(bytes, local + 22)? != uncompressed)
        {
            return Err(invalid());
        }
        let interval_end = if flags & 8 != 0 {
            let descriptor = if bytes.get(finish..finish + 4) == Some(b"PK\x07\x08") {
                finish.checked_add(4).ok_or_else(invalid)?
            } else {
                finish
            };
            let descriptor_end = descriptor.checked_add(12).ok_or_else(invalid)?;
            if descriptor_end > start
                || u32_at(bytes, descriptor)? != u32_at(bytes, at + 16)?
                || u32_at(bytes, descriptor + 4)? != compressed
                || u32_at(bytes, descriptor + 8)? != uncompressed
            {
                return Err(invalid());
            }
            descriptor_end
        } else {
            finish
        };
        ranges.push((local, interval_end));
        at = next;
    }
    if at != end {
        return Err(invalid());
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|p| p[0].1 > p[1].0) {
        return Err(invalid());
    }
    for name in names.keys() {
        for (at, _) in name.match_indices('/') {
            if names.get(&name[..at]) == Some(&false) {
                return Err(invalid());
            }
        }
    }
    Ok(count)
}
fn fingerprint(
    file: &File,
    receipt: &PackageDownload,
    cancel: &CancellationToken,
    expanded_limit: u64,
) -> Result<Fingerprint> {
    let bytes = snapshot(file, receipt, cancel)?;
    let count = layout(&bytes, cancel, expanded_limit)?;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid())?;
    if archive.len() != count {
        return Err(invalid());
    }
    let mut entries = BTreeMap::new();
    let mut expanded = 0u64;
    let mut files = 0;
    for index in 0..count {
        check(cancel)?;
        let mut entry = archive.by_index(index).map_err(|_| invalid())?;
        if entry.encrypted()
            || entry.is_symlink()
            || !matches!(
                entry.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            )
        {
            return Err(invalid());
        }
        let directory = entry.is_dir();
        let name = safe_name(entry.name_raw(), directory)?;
        let mut size = 0u64;
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            check(cancel)?;
            let n = entry.read(&mut buffer).map_err(|_| invalid())?;
            if n == 0 {
                break;
            }
            size = size.checked_add(n as u64).ok_or_else(invalid)?;
            expanded = expanded.checked_add(n as u64).ok_or_else(invalid)?;
            if size > entry.size() || expanded > expanded_limit {
                return Err(invalid());
            }
            hash.update(&buffer[..n]);
        }
        if size != entry.size() || (directory && size != 0) {
            return Err(invalid());
        }
        if !directory {
            files += 1;
        }
        if entries
            .insert(
                name,
                Entry {
                    directory,
                    size,
                    sha256: hex::encode(hash.finalize()),
                },
            )
            .is_some()
        {
            return Err(invalid());
        }
    }
    Ok(Fingerprint {
        entries,
        expanded,
        files,
    })
}
#[cfg(test)]
mod tests;

/// Diagnostic only: exact file entries remain significant; directory-only delta
/// is an observation, never a semantic identity or an admission decision.
#[cfg(feature = "write-probe")]
pub(crate) fn diagnostic_archive_comparison(
    source: &File,
    source_receipt: &PackageDownload,
    source_root: &str,
    downloaded: &File,
    downloaded_receipt: &PackageDownload,
    downloaded_root: &str,
    cancel: &CancellationToken,
) -> Result<serde_json::Value> {
    validate_root(source_root)?;
    validate_root(downloaded_root)?;
    let a = bind_root(
        fingerprint(source, source_receipt, cancel, MAX_EXPANDED)?,
        source_root,
    )?;
    let b = bind_root(
        fingerprint(downloaded, downloaded_receipt, cancel, MAX_EXPANDED)?,
        downloaded_root,
    )?;
    check(cancel)?;
    let files_equal = a
        .entries
        .iter()
        .filter(|(_, entry)| !entry.directory)
        .eq(b.entries.iter().filter(|(_, entry)| !entry.directory));
    let all_equal = a.entries == b.entries;
    Ok(serde_json::json!({
        "comparison_available":true,
        "source_entries":a.entries.len(), "downloaded_entries":b.entries.len(),
        "source_files":a.files,"downloaded_files":b.files,
        "source_directories":a.entries.len()-a.files,"downloaded_directories":b.entries.len()-b.files,
        "source_expanded_bytes":a.expanded,"downloaded_expanded_bytes":b.expanded,
        "exact_file_paths_sizes_hashes_equal":files_equal,
        "all_entries_equal":all_equal,
        "directory_only_delta":files_equal && !all_equal
    }))
}
