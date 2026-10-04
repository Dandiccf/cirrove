//! Read-only registered fixture observer. Never an application-fidelity claim.
use super::*;
use anyhow::bail;
use cirrove_core::upload::PackageSemanticIdentity;
use cirrove_icloud::FixtureRepresentation;
use std::os::unix::fs::MetadataExt;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    version: u32,
    run: Uuid,
    account: Uuid,
    session_directory: PathBuf,
    settings_sha256: String,
    parent: DriveEntry,
    document: DriveEntry,
    format: String,
    representation: FixtureRepresentation,
    source: PathBuf,
    source_size: u64,
    source_sha256: String,
    source_root: String,
    expected_root: String,
    semantic: Option<PackageSemanticIdentity>,
}
fn hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
// Descriptor traversal rejects symlinks in EVERY component, including ancestors.
fn open_private(path: &Path, directory: bool, max: u64) -> Result<File> {
    use std::path::Component;
    ensure!(path.is_absolute(), "fixture path must be absolute");
    let parts = path
        .components()
        .skip(1)
        .map(|part| match part {
            Component::Normal(p) => Ok(p),
            _ => bail!("fixture path is not canonical"),
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!parts.is_empty(), "fixture path missing");
    let mut parent = File::open("/")?;
    for (index, part) in parts.iter().enumerate() {
        use rustix::fs::{Mode, OFlags};
        let last = index + 1 == parts.len();
        let flags = OFlags::RDONLY
            | OFlags::CLOEXEC
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | if !last || directory {
                OFlags::DIRECTORY
            } else {
                OFlags::empty()
            };
        parent = File::from(
            rustix::fs::openat(&parent, *part, flags, Mode::empty())
                .map_err(|_| anyhow::anyhow!("fixture private path refused"))?,
        );
    }
    let meta = parent.metadata()?;
    ensure!(
        meta.uid() == std::fs::metadata("/proc/self")?.uid()
            && meta.permissions().mode() & 0o077 == 0
            && if directory {
                meta.is_dir()
            } else {
                meta.is_file() && meta.len() <= max
            },
        "fixture ownership or bounds refused"
    );
    Ok(parent)
}
fn same_directory(path: &Path, held: &File) -> Result<()> {
    let current = open_private(path, true, 0)?.metadata()?;
    let original = held.metadata()?;
    ensure!(
        current.dev() == original.dev() && current.ino() == original.ino(),
        "fixture directory identity changed"
    );
    Ok(())
}
fn decode(bytes: &[u8], expected_hash: &str) -> Result<Fixture> {
    ensure!(
        bytes.len() <= 32 * 1024
            && hex_digest(expected_hash)
            && hex::encode(Sha256::digest(bytes)) == expected_hash,
        "registered fixture manifest changed"
    );
    serde_json::from_slice(bytes).map_err(|_| anyhow::anyhow!("fixture schema refused"))
}
fn validate(f: &Fixture) -> Result<()> {
    let extension = match f.format.as_str() {
        "pages" => "pages",
        "numbers" => "numbers",
        "keynote" => "key",
        _ => bail!("fixture format unsupported"),
    };
    ensure!(
        f.version == 1
            && !f.run.is_nil()
            && !f.account.is_nil()
            && f.parent.kind == "FOLDER"
            && f.parent.zone == "com.apple.CloudDocs"
            && f.parent.name == format!("Cirrove-Native-{}", f.run)
            && f.parent
                .drivewsid
                .starts_with("FOLDER::com.apple.CloudDocs::")
            && f.document.kind == "FILE"
            && f.document.zone == "com.apple.CloudDocs"
            && f.document.parent_id == f.parent.drivewsid
            && f.document.drivewsid == format!("FILE::com.apple.CloudDocs::{}", f.document.docwsid)
            && !f.document.docwsid.is_empty()
            && !f.document.etag.is_empty()
            && !f.document.etag.contains('*')
            && f.document.extension == extension
            && f.document.name.contains(&f.run.to_string())
            && f.source_root.ends_with(&format!(".{extension}"))
            && !f.source_root.contains(['/', '\\'])
            && f.source_root.len() <= 255
            && f.expected_root.ends_with(&format!(".{extension}"))
            && f.expected_root == format!("{}.{}", f.document.name, extension)
            && !f.expected_root.contains(['/', '\\'])
            && f.expected_root.len() <= 255
            && f.source_size > 0
            && f.source_size <= LIMIT
            && hex_digest(&f.source_sha256)
            && hex_digest(&f.settings_sha256)
            && f.session_directory.starts_with("/var/tmp")
            && f.session_directory.components().count() > 3,
        "fixture ownership registration refused"
    );
    match (&f.representation, &f.semantic) {
        (FixtureRepresentation::Data, None) => {}
        (FixtureRepresentation::Package, Some(proof)) => {
            proof.validate()?;
            ensure!(proof.version == 2, "fixture requires explicit semantic v2");
        }
        _ => bail!("fixture representation proof mismatch"),
    }
    Ok(())
}
fn exact_account(f: &Fixture, actual: &str) -> Result<()> {
    ensure!(
        actual == f.account.to_string(),
        "fixture account identity differs"
    );
    Ok(())
}
fn account_binding(f: &Fixture, accounts: &[Account]) -> Result<Account> {
    ensure!(
        accounts.len() == 1,
        "isolated fixture account inventory changed"
    );
    let a = accounts[0].clone();
    exact_account(f, &a.id)?;
    ensure!(
        matches!(a.registration, AppRegistration::ICloud),
        "fixture account provider differs"
    );
    Ok(a)
}
fn proof(f: &Fixture, file: &File, receipt: &PackageDownload, source: bool) -> Result<()> {
    if source || f.representation == FixtureRepresentation::Data {
        ensure!(
            receipt.size == f.source_size && receipt.sha256 == f.source_sha256,
            "fixture content digest differs"
        );
    }
    if let Some(expected) = &f.semantic {
        let actual = cirrove_icloud::package_archive_semantic_identity_versioned(
            file,
            receipt,
            if source {
                &f.source_root
            } else {
                &f.expected_root
            },
            expected.version,
            &CancellationToken::new(),
        )?;
        ensure!(&actual == expected, "fixture semantic content differs");
    }
    Ok(())
}
/// Public only under icloud-write-probe. All output is identity/digest evidence.
pub async fn icloud_owned_fixture_verify(
    path: &Path,
    manifest_sha256: &str,
) -> Result<serde_json::Value> {
    let directory = path.parent().context("fixture directory missing")?;
    let directory_guard = open_private(directory, true, 0)?;
    let mut bytes = Vec::new();
    open_private(path, false, 32 * 1024)?
        .take(32 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    let f = decode(&bytes, manifest_sha256)?;
    validate(&f)?;
    let _session_directory = open_private(&f.session_directory, true, 0)?;
    let _state = open_private(&f.session_directory.join("state"), true, 0)?;
    let settings_path = f.session_directory.join("state/accounts.json");
    let mut settings_bytes = Vec::new();
    open_private(&settings_path, false, 256 * 1024)?
        .take(256 * 1024 + 1)
        .read_to_end(&mut settings_bytes)?;
    ensure!(
        settings_bytes.len() <= 256 * 1024
            && hex::encode(Sha256::digest(&settings_bytes)) == f.settings_sha256,
        "fixture settings changed"
    );
    let settings: Settings = serde_json::from_slice(&settings_bytes)
        .map_err(|_| anyhow::anyhow!("fixture account settings refused"))?;
    ensure!(settings.version == 2, "fixture account schema unsupported");
    let account = account_binding(&f, &settings.accounts)?;
    let mut source = open_private(&f.source, false, LIMIT)?;
    let source_receipt = PackageDownload {
        size: source.metadata()?.len(),
        sha256: sha256(&mut source)?,
    };
    proof(&f, &source, &source_receipt, true)?;
    same_directory(directory, &directory_guard)?;
    let attempt = verification_directory(directory)?;
    let attempt_guard = open_private(&attempt, true, 0)?;
    manifest(&attempt, f.run, "owned-fixture-read-only")?;
    // Sealed saved session only: this helper never calls sign_in/account_login.
    let mut remote = session(&f.session_directory, &account)
        .await
        .map_err(|_| anyhow::anyhow!("fixture sealed session unavailable"))?;
    let output = attempt.join("download.bin");
    let staged = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)?;
    let mut sink = DiskSink(tokio::fs::File::from_std(staged.try_clone()?));
    let receipt = remote
        .read_owned_fixture(
            &f.parent,
            &f.document,
            f.representation,
            &mut sink,
            &CancellationToken::new(),
        )
        .await?;
    sink.0.flush().await?;
    sink.0.sync_all().await?;
    proof(&f, &staged, &receipt, false)?;
    // Evidence output is withheld if the preregistration or original archive moved.
    let mut end = Vec::new();
    open_private(path, false, 32 * 1024)?
        .take(32 * 1024 + 1)
        .read_to_end(&mut end)?;
    let _ = decode(&end, manifest_sha256)?;
    let mut final_source = open_private(&f.source, false, LIMIT)?;
    ensure!(
        sha256(&mut final_source)? == f.source_sha256,
        "fixture source changed during observation"
    );
    let result = serde_json::json!({"run":f.run,"account":f.account,"parent":f.parent.drivewsid,"item":f.document.drivewsid,"etag":f.document.etag,"representation":f.representation,"format":f.format,"size":receipt.size,"sha256":receipt.sha256,"semantic":f.semantic,"manifest_sha256":manifest_sha256,"content_identity_verified":true,"gui_fidelity_verified":false,"cloud_mutated":false});
    same_directory(directory, &directory_guard)?;
    same_directory(&attempt, &attempt_guard)?;
    record(&attempt.join("receipt.json"), &result)?;
    Ok(result)
}

mod receipt_bound;
pub use receipt_bound::{
    icloud_owned_fuse_capture_verify, icloud_owned_fuse_receipt_verify,
    icloud_owned_fuse_source_verify, icloud_owned_keynote_import_receipt_verify,
    icloud_owned_keynote_source_verify, icloud_owned_numbers_data_receipt_verify,
    icloud_owned_numbers_data_source_verify, icloud_owned_receipt_verify,
};

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    fn fixture() -> Result<(Vec<u8>, Fixture)> {
        let run = Uuid::new_v4();
        let account = Uuid::new_v4();
        let name = format!("Owned-{run}");
        let bytes = serde_json::to_vec(
            &serde_json::json!({"version":1,"run":run,"account":account,
            "session_directory":"/var/tmp/owned-fixture-session","settings_sha256":"a".repeat(64),
            "parent":{"drivewsid":"FOLDER::com.apple.CloudDocs::owned","name":format!("Cirrove-Native-{run}"),"zone":"com.apple.CloudDocs","type":"FOLDER"},
            "document":{"drivewsid":"FILE::com.apple.CloudDocs::doc","docwsid":"doc","parentId":"FOLDER::com.apple.CloudDocs::owned","name":name,"extension":"pages","zone":"com.apple.CloudDocs","type":"FILE","etag":"r1","size":3},
            "format":"pages","representation":"data","source":"/var/tmp/owned-source.pages","source_size":3,"source_sha256":hex::encode(Sha256::digest(b"abc")),"source_root":"Export.pages","expected_root":format!("{name}.pages"),"semantic":null}),
        )?;
        let f = decode(&bytes, &hex::encode(Sha256::digest(&bytes)))?;
        Ok((bytes, f))
    }
    #[test]
    fn owned_fixture_registration_rejects_changed_manifest_account_parent_format() -> Result<()> {
        let (mut bytes, mut f) = fixture()?;
        validate(&f)?;
        let original_hash = hex::encode(Sha256::digest(&bytes));
        bytes.push(b' ');
        assert!(decode(&bytes, &original_hash).is_err());
        exact_account(&f, &f.account.to_string())?;
        assert!(exact_account(&f, &Uuid::new_v4().to_string()).is_err());
        f.document.parent_id = "FOLDER::com.apple.CloudDocs::foreign".into();
        assert!(validate(&f).is_err());
        let (_, mut f) = fixture()?;
        f.format = "pdf".into();
        assert!(validate(&f).is_err());
        let (_, mut f) = fixture()?;
        f.representation = FixtureRepresentation::Package;
        assert!(validate(&f).is_err());
        Ok(())
    }
    #[test]
    fn owned_fixture_private_paths_and_altered_source_fail_closed() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
        let path = root.path().join("source");
        std::fs::write(&path, b"abc")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let file = open_private(&path, false, LIMIT)?;
        let (_, f) = fixture()?;
        proof(
            &f,
            &file,
            &PackageDownload {
                size: 3,
                sha256: hex::encode(Sha256::digest(b"abc")),
            },
            true,
        )?;
        assert!(
            proof(
                &f,
                &file,
                &PackageDownload {
                    size: 3,
                    sha256: hex::encode(Sha256::digest(b"xyz"))
                },
                true
            )
            .is_err()
        );
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(root.path(), &alias)?;
        assert!(open_private(&alias.join("source"), false, LIMIT).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        assert!(open_private(&path, false, LIMIT).is_err());
        Ok(())
    }
    fn zip_archive(name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        crc = !crc;
        let size = u32::try_from(bytes.len()).unwrap();
        let len = u16::try_from(name.len()).unwrap();
        let mut out = Vec::new();
        out.extend(0x04034b50u32.to_le_bytes());
        for value in [20u16, 0, 0, 0, 33] {
            out.extend(value.to_le_bytes());
        }
        for value in [crc, size, size] {
            out.extend(value.to_le_bytes());
        }
        for value in [len, 0] {
            out.extend(value.to_le_bytes());
        }
        out.extend(name.as_bytes());
        out.extend(bytes);
        let offset = u32::try_from(out.len()).unwrap();
        out.extend(0x02014b50u32.to_le_bytes());
        for value in [20u16, 20, 0, 0, 0, 33] {
            out.extend(value.to_le_bytes());
        }
        for value in [crc, size, size] {
            out.extend(value.to_le_bytes());
        }
        for value in [len, 0, 0, 0, 0] {
            out.extend(value.to_le_bytes());
        }
        for value in [0u32, 0] {
            out.extend(value.to_le_bytes());
        }
        out.extend(name.as_bytes());
        let central = u32::try_from(out.len()).unwrap() - offset;
        out.extend(0x06054b50u32.to_le_bytes());
        for value in [0u16, 0, 1, 1] {
            out.extend(value.to_le_bytes());
        }
        for value in [central, offset] {
            out.extend(value.to_le_bytes());
        }
        out.extend(0u16.to_le_bytes());
        out
    }

    #[test]
    fn owned_fixture_different_source_and_remote_roots_preserve_normalized_proof() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (_, mut f) = fixture()?;
        let source_path = root.path().join("source");
        let remote_path = root.path().join("remote");
        let source = zip_archive(
            &format!("{}/Index/Document.iwa", f.source_root),
            b"fixed own content",
        );
        let remote = zip_archive(
            &format!("{}/Index/Document.iwa", f.expected_root),
            b"fixed own content",
        );
        std::fs::write(&source_path, &source)?;
        std::fs::write(&remote_path, &remote)?;
        let a = File::open(&source_path)?;
        let b = File::open(&remote_path)?;
        let ar = PackageDownload {
            size: source.len() as u64,
            sha256: hex::encode(Sha256::digest(&source)),
        };
        let br = PackageDownload {
            size: remote.len() as u64,
            sha256: hex::encode(Sha256::digest(&remote)),
        };
        f.representation = FixtureRepresentation::Package;
        f.source_size = ar.size;
        f.source_sha256 = ar.sha256.clone();
        f.semantic = Some(cirrove_icloud::package_archive_semantic_identity_versioned(
            &a,
            &ar,
            &f.source_root,
            2,
            &CancellationToken::new(),
        )?);
        validate(&f)?;
        proof(&f, &a, &ar, true)?;
        proof(&f, &b, &br, false)?;
        // Keep the exact remote root and valid archive, alter only content.
        // A digest-only transport success must not bypass semantic comparison.
        let changed = zip_archive(
            &format!("{}/Index/Document.iwa", f.expected_root),
            b"different own content",
        );
        std::fs::write(&remote_path, &changed)?;
        let changed_file = File::open(&remote_path)?;
        let changed_receipt = PackageDownload {
            size: changed.len() as u64,
            sha256: hex::encode(Sha256::digest(&changed)),
        };
        assert!(proof(&f, &changed_file, &changed_receipt, false).is_err());
        f.source_root = "Wrong.pages".into();
        assert!(proof(&f, &a, &ar, true).is_err());
        f.expected_root = "Wrong.pages".into();
        assert!(proof(&f, &b, &br, false).is_err());
        Ok(())
    }
}
