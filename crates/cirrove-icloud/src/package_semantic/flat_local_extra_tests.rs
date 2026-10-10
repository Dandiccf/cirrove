#![allow(clippy::unwrap_used)]
use super::*;
use std::{io::Write, os::unix::fs::PermissionsExt};
fn p16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn p32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn r16(b: &[u8], at: usize) -> usize {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap()) as usize
}
fn r32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn mirror(us: u64, cs: u64) -> Vec<u8> {
    let mut v = vec![1, 0, 16, 0];
    v.extend(us.to_le_bytes());
    v.extend(cs.to_le_bytes());
    v
}
// Real zip writer provides valid CRC/compression. Only named structural fields
// are adjusted; each finished ZIP has coherent classic central offsets/EOCD.
fn archive(prefix: &str, arm: u8, count: usize) -> Vec<u8> {
    let mut locals = Vec::new();
    let mut centrals = Vec::new();
    for relative in ["Index/Document.iwa", "Metadata/Info.plist"]
        .iter()
        .take(count)
    {
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let method = if arm == 10 {
            zip::CompressionMethod::Deflated
        } else {
            zip::CompressionMethod::Stored
        };
        z.start_file(
            format!("{prefix}{relative}"),
            zip::write::SimpleFileOptions::default().compression_method(method),
        )
        .unwrap();
        z.write_all(b"owned flat bytes").unwrap();
        let raw = z.finish().unwrap().into_inner();
        let end = raw.len() - 22;
        let center = r32(&raw, end + 16) as usize;
        let center_len = r32(&raw, end + 12) as usize;
        assert_eq!(
            r16(&raw, 6),
            0,
            "fixture must use flags0 before descriptor arm"
        );
        assert_eq!(
            r16(&raw, 28),
            0,
            "fixture must have no existing local extras"
        );
        assert_eq!(
            r16(&raw, center + 30),
            0,
            "fixture must have no existing central extras"
        );
        let us = r32(&raw, center + 24);
        let cs = r32(&raw, center + 20);
        let crc = r32(&raw, center + 16);
        let header = 30 + r16(&raw, 26);
        let mut extra = if arm == 0 || arm == 9 {
            Vec::new()
        } else {
            mirror(u64::from(us), u64::from(cs))
        };
        match arm {
            2 => extra[4..12].copy_from_slice(&(u64::from(us) + 1).to_le_bytes()),
            3 => extra[12..20].copy_from_slice(&(u64::from(cs) + 1).to_le_bytes()),
            4 => extra.extend(mirror(u64::from(us), u64::from(cs))),
            5 => {
                p16(&mut extra, 2, 8);
                extra.truncate(12);
            }
            6 => {
                p16(&mut extra, 2, 24);
                extra.extend(0u64.to_le_bytes());
            }
            7 => {
                extra.pop();
            }
            8 => {
                extra.truncate(3);
            }
            _ => {}
        }
        let mut local = raw[..header].to_vec();
        p16(&mut local, 28, u16::try_from(extra.len()).unwrap());
        if arm == 11 {
            p16(&mut local, 6, 8);
            p32(&mut local, 14, 0);
            p32(&mut local, 18, 0);
            p32(&mut local, 22, 0);
        }
        if arm == 12 {
            p32(&mut local, 22, u32::MAX);
        }
        local.extend(extra);
        local.extend(&raw[header..center]);
        if arm == 11 {
            local.extend(0x08074b50u32.to_le_bytes());
            local.extend(crc.to_le_bytes());
            local.extend(cs.to_le_bytes());
            local.extend(us.to_le_bytes());
        }
        let mut central = raw[center..center + center_len].to_vec();
        p32(&mut central, 42, u32::try_from(locals.len()).unwrap());
        if arm == 9 {
            let extra = mirror(u64::from(us), u64::from(cs));
            p16(&mut central, 30, u16::try_from(extra.len()).unwrap());
            central.extend(extra);
        }
        if arm == 11 {
            p16(&mut central, 8, 8);
        }
        if arm == 13 {
            p32(&mut central, 24, u32::MAX);
        }
        locals.extend(local);
        centrals.extend(central);
    }
    let start = u32::try_from(locals.len()).unwrap();
    let len = u32::try_from(centrals.len()).unwrap();
    locals.extend(centrals);
    locals.extend(0x06054b50u32.to_le_bytes());
    for v in [
        0u16,
        0,
        u16::try_from(count).unwrap(),
        u16::try_from(count).unwrap(),
    ] {
        locals.extend(v.to_le_bytes());
    }
    locals.extend(len.to_le_bytes());
    locals.extend(start.to_le_bytes());
    locals.extend(0u16.to_le_bytes());
    locals
}
fn stage(bytes: &[u8]) -> (File, PackageDownload, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap().keep();
    let path = dir.join("owned.zip");
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    (
        File::open(&path).unwrap(),
        PackageDownload {
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
        },
        path,
    )
}
fn library_reads(bytes: &[u8], count: usize) {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    assert_eq!(z.len(), count);
    for n in 0..count {
        let mut body = Vec::new();
        z.by_index(n).unwrap().read_to_end(&mut body).unwrap();
        assert_eq!(body, b"owned flat bytes");
    }
}
#[test]
fn flat_native_local_size_mirror_accepts_two_redundant_pairs_without_rewriting() {
    let plain = archive("", 0, 2);
    let mirrored = archive("", 1, 2);
    library_reads(&plain, 2);
    library_reads(&mirrored, 2);
    let (a, ar, _) = stage(&plain);
    let (b, br, path) = stage(&mirrored);
    let expected =
        package_flat_archive_semantic_identity_v2(&a, &ar, &CancellationToken::new()).unwrap();
    let result = package_flat_archive_semantic_identity_v2(&b, &br, &CancellationToken::new());
    assert_eq!(std::fs::read(&path).unwrap(), mirrored);
    assert_eq!(b.metadata().unwrap().len(), br.size);
    assert_eq!(
        hex::encode(Sha256::digest(std::fs::read(&path).unwrap())),
        br.sha256
    );
    let actual = result.expect("redundant local ZIP64 size mirrors rejected by existing flat API");
    assert_eq!(actual, expected);
    assert_ne!(ar.sha256, br.sha256);
    assert_eq!((actual.version, actual.files, actual.entries), (2, 2, 5));
}
#[test]
fn flat_native_local_size_mirror_refuses_malformed_and_nonclassic_modes() {
    // Deflate/descriptor archives are independently valid before flat policy.
    for arm in [10, 11] {
        library_reads(&archive("", arm, 1), 1);
    }
    for arm in 2..=13 {
        let bytes = archive("", arm, 1);
        let (file, receipt, path) = stage(&bytes);
        let result =
            package_flat_archive_semantic_identity_v2(&file, &receipt, &CancellationToken::new());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert!(result.is_err(), "unsafe redundant extra arm {arm}");
    }
}
#[test]
fn flat_native_local_size_mirror_never_relaxes_wrapped_apis_or_raw_receipts() {
    let plain = archive("Owned.numbers/", 0, 1);
    let mirrored = archive("Owned.numbers/", 1, 1);
    let (plain, pr, _) = stage(&plain);
    let (file, receipt, path) = stage(&mirrored);
    for version in [1, 2] {
        package_archive_semantic_identity_versioned(
            &plain,
            &pr,
            "Owned.numbers",
            version,
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(
            package_archive_semantic_identity_versioned(
                &file,
                &receipt,
                "Owned.numbers",
                version,
                &CancellationToken::new()
            )
            .is_err()
        );
    }
    assert!(
        package_archive_semantic_identity(
            &file,
            &receipt,
            "Owned.numbers",
            &CancellationToken::new()
        )
        .is_err()
    );
    assert!(
        compare_package_archives(&plain, &pr, &file, &receipt, &CancellationToken::new()).is_err()
    );
    let flat = archive("", 1, 1);
    let (file, receipt, _) = stage(&flat);
    let bad = PackageDownload {
        size: receipt.size,
        sha256: "a".repeat(64),
    };
    assert!(
        package_flat_archive_semantic_identity_v2(&file, &bad, &CancellationToken::new()).is_err()
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        package_flat_archive_semantic_identity_v2(&file, &receipt, &cancel),
        Err(ProviderError::Cancelled)
    ));
    assert_eq!(std::fs::read(path).unwrap(), mirrored);
}
