//! Stable ZIP export metadata without decompressing or rewriting document data.
use crate::PackageDownload;
use cirrove_core::{CancellationToken, ProviderError};
use sha2::{Digest, Sha256};
use std::{fs::File, os::unix::fs::FileExt};
type Result<T> = std::result::Result<T, ProviderError>;
fn invalid() -> ProviderError {
    ProviderError::Protocol("unsupported or invalid iCloud package archive")
}
fn read<const N: usize>(file: &File, offset: u64) -> Result<[u8; N]> {
    let mut bytes = [0; N];
    file.read_exact_at(&mut bytes, offset)
        .map_err(|_| invalid())?;
    Ok(bytes)
}
fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
fn digest(file: &File, size: u64, cancel: &CancellationToken) -> Result<String> {
    let mut hash = Sha256::new();
    let mut offset = 0;
    let mut buffer = vec![0; 1024 * 1024];
    while offset < size {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let count = (size - offset).min(buffer.len() as u64) as usize;
        file.read_exact_at(&mut buffer[..count], offset)
            .map_err(|_| invalid())?;
        hash.update(&buffer[..count]);
        offset += count as u64;
    }
    Ok(hex::encode(hash.finalize()))
}

/// Canonicalize only DOS timestamps of empty ZIP directories in private staging.
/// Apple's package generator changes these even for the same source revision.
/// File bytes, compression, names, order and file timestamps remain untouched.
/// Only a successful receipt may be published; an error can leave private staging
/// partially modified. ZIP64 and oversized central directories are refused here.
pub fn canonical_export(
    file: &File,
    receipt: &PackageDownload,
    cancel: &CancellationToken,
) -> Result<PackageDownload> {
    if file.metadata().map_err(|_| invalid())?.len() != receipt.size
        || digest(file, receipt.size, cancel)? != receipt.sha256
    {
        return Err(ProviderError::VersionChanged);
    }
    let tail_len = receipt.size.min(65557) as usize;
    let mut tail = vec![0; tail_len];
    let tail_start = receipt.size - tail_len as u64;
    file.read_exact_at(&mut tail, tail_start)
        .map_err(|_| invalid())?;
    let end = (0..tail_len.saturating_sub(21))
        .rev()
        .find(|&i| {
            tail[i..].starts_with(b"PK\x05\x06")
                && i + 22 + u16_at(&tail, i + 20) as usize == tail_len
        })
        .ok_or_else(invalid)?;
    let footer = &tail[end..];
    let count = u16_at(footer, 10);
    let central_size = u32_at(footer, 12);
    let central_start = u32_at(footer, 16);
    // Bound library metadata allocation before it parses the central directory.
    if u16_at(footer, 4) != 0
        || u16_at(footer, 6) != 0
        || u16_at(footer, 8) != count
        || count == 0
        || count > 10_000
        || central_size > 16 * 1024 * 1024
        || u64::from(central_start) + u64::from(central_size) != tail_start + end as u64
    {
        return Err(invalid());
    }
    let mut zip =
        zip::ZipArchive::new(file.try_clone().map_err(|_| invalid())?).map_err(|_| invalid())?;
    if zip.len() != count as usize || zip.central_directory_start() != u64::from(central_start) {
        return Err(invalid());
    }
    let mut intervals = Vec::with_capacity(zip.len());
    let mut patches = Vec::new();
    for index in 0..zip.len() {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let entry = zip.by_index_raw(index).map_err(|_| invalid())?;
        let local = entry.header_start();
        let central = entry.central_header_start();
        let header = read::<30>(file, local)?;
        let directory = read::<46>(file, central)?;
        if !header.starts_with(b"PK\x03\x04")
            || !directory.starts_with(b"PK\x01\x02")
            || u16_at(&header, 26) as usize != entry.name_raw().len()
            || u16_at(&directory, 28) as usize != entry.name_raw().len()
        {
            return Err(invalid());
        }
        let mut name = vec![0; entry.name_raw().len()];
        file.read_exact_at(&mut name, local + 30)
            .map_err(|_| invalid())?;
        if name != entry.name_raw() {
            return Err(invalid());
        }
        let data_start = local
            .checked_add(30 + name.len() as u64 + u64::from(u16_at(&header, 28)))
            .ok_or_else(invalid)?;
        let data_end = data_start
            .checked_add(entry.compressed_size())
            .ok_or_else(invalid)?;
        let central_end = central
            .checked_add(
                46 + name.len() as u64
                    + u64::from(u16_at(&directory, 30))
                    + u64::from(u16_at(&directory, 32)),
            )
            .ok_or_else(invalid)?;
        if data_end > u64::from(central_start)
            || central < u64::from(central_start)
            || central_end > u64::from(central_start) + u64::from(central_size)
        {
            return Err(invalid());
        }
        intervals.push((local, data_end));
        if entry.is_dir() {
            let method = u16_at(&directory, 10);
            let empty_payload = match (method, entry.compressed_size()) {
                (0, 0) => true,
                (8, 2) => read::<2>(file, data_start)? == [3, 0],
                _ => false,
            };
            if entry.size() != 0
                || entry.crc32() != 0
                || !empty_payload
                || u16_at(&header, 8) != method
                || u16_at(&header, 6) != u16_at(&directory, 8)
                || u16_at(&header, 6) & 1 != 0
                || ![0, entry.compressed_size()].contains(&u64::from(u32_at(&header, 18)))
                || u32_at(&header, 22) != 0
            {
                return Err(invalid());
            }
            patches.push((local + 10, central + 12));
        }
    }
    intervals.sort_unstable();
    if intervals.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(invalid());
    }
    drop(zip);
    // 1980-01-01 00:00:00, represented as DOS time/date (little endian).
    for (local, central) in patches {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        file.write_all_at(&[0, 0, 33, 0], local)
            .map_err(|_| invalid())?;
        file.write_all_at(&[0, 0, 33, 0], central)
            .map_err(|_| invalid())?;
    }
    let sha256 = digest(file, receipt.size, cancel)?;
    if cancel.is_cancelled() {
        return Err(ProviderError::Cancelled);
    }
    Ok(PackageDownload {
        size: receipt.size,
        sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    fn archive(year: u16) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let directory = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .last_modified_time(
                zip::DateTime::from_date_and_time(year, 1, 1, 0, 0, 0).expect("date"),
            );
        zip.add_directory("Metadata/", directory)
            .expect("directory");
        let file = directory.last_modified_time(
            zip::DateTime::from_date_and_time(2020, 2, 3, 4, 5, 6).expect("file date"),
        );
        zip.start_file("Metadata/document", file).expect("file");
        zip.write_all(b"unchanged document data").expect("bytes");
        zip.finish().expect("zip").into_inner()
    }
    #[test]
    fn generated_directory_times_produce_one_export_without_changing_file_data() {
        let temp = tempfile::tempdir().expect("temp");
        let mut results = Vec::new();
        for year in [2025, 2026] {
            let bytes = archive(year);
            let file = tempfile::tempfile_in(temp.path()).expect("file");
            file.write_all_at(&bytes, 0).expect("write");
            let raw = PackageDownload {
                size: bytes.len() as u64,
                sha256: hex::encode(Sha256::digest(&bytes)),
            };
            let result =
                canonical_export(&file, &raw, &CancellationToken::new()).expect("canonical export");
            let mut after = vec![0; bytes.len()];
            file.read_exact_at(&mut after, 0).expect("reread");
            assert_eq!(result.sha256, hex::encode(Sha256::digest(&after)));
            let mut original = zip::ZipArchive::new(Cursor::new(bytes)).expect("original");
            let mut normalized =
                zip::ZipArchive::new(Cursor::new(after.clone())).expect("normalized");
            let mut original = original.by_index_raw(1).expect("original file");
            let mut normalized = normalized.by_index_raw(1).expect("normalized file");
            assert_eq!(original.last_modified(), normalized.last_modified());
            let mut a = Vec::new();
            let mut b = Vec::new();
            std::io::Read::read_to_end(&mut original, &mut a).expect("payload");
            std::io::Read::read_to_end(&mut normalized, &mut b).expect("payload");
            assert_eq!(a, b);
            results.push(after);
        }
        assert_eq!(
            results[0], results[1],
            "directory export timestamps must not change the cache identity"
        );
    }
    #[test]
    fn empty_deflated_directory_is_normalized_without_decompression() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer
            .add_directory("Directory/", options)
            .expect("directory");
        let mut bytes = writer.finish().expect("zip").into_inner();
        let old_end = bytes.len() - 22;
        let central = u32_at(&bytes, old_end + 16) as usize;
        bytes.splice(central..central, [3, 0]);
        bytes[8..10].copy_from_slice(&8u16.to_le_bytes());
        bytes[18..22].copy_from_slice(&2u32.to_le_bytes());
        bytes[central + 2 + 10..central + 2 + 12].copy_from_slice(&8u16.to_le_bytes());
        bytes[central + 2 + 20..central + 2 + 24].copy_from_slice(&2u32.to_le_bytes());
        let end = bytes.len() - 22;
        bytes[end + 16..end + 20].copy_from_slice(&((central + 2) as u32).to_le_bytes());
        let temp = tempfile::tempdir().expect("temp");
        let file = tempfile::tempfile_in(temp.path()).expect("file");
        file.write_all_at(&bytes, 0).expect("write");
        let receipt = PackageDownload {
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(&bytes)),
        };
        canonical_export(&file, &receipt, &CancellationToken::new())
            .expect("empty compressed directory");
        assert_eq!(
            read::<2>(&file, central as u64).expect("compressed payload"),
            [3, 0]
        );
    }

    #[test]
    fn invalid_archive_or_wrong_receipt_never_returns_an_export() {
        let temp = tempfile::tempdir().expect("temp");
        for bytes in [
            Vec::new(),
            b"not a package".to_vec(),
            archive(2026)[..30].to_vec(),
        ] {
            let file = tempfile::tempfile_in(temp.path()).expect("file");
            file.write_all_at(&bytes, 0).expect("write");
            let raw = PackageDownload {
                size: bytes.len() as u64,
                sha256: hex::encode(Sha256::digest(&bytes)),
            };
            assert!(canonical_export(&file, &raw, &CancellationToken::new()).is_err());
        }
        let bytes = archive(2026);
        let file = tempfile::tempfile_in(temp.path()).expect("file");
        file.write_all_at(&bytes, 0).expect("write");
        let wrong = PackageDownload {
            size: bytes.len() as u64,
            sha256: "wrong".into(),
        };
        assert!(matches!(
            canonical_export(&file, &wrong, &CancellationToken::new()),
            Err(ProviderError::VersionChanged)
        ));
    }
}
