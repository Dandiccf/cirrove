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
    #[serde(deserialize_with = "required_source_root")]
    source_root: Option<String>,
    expected_root: String,
    semantic: Option<PackageSemanticIdentity>,
}
fn required_source_root<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(deserializer)
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
        "xlsx" => "xlsx",
        _ => bail!("fixture format unsupported"),
    };
    if f.format == "xlsx" {
        ensure!(
            f.representation == FixtureRepresentation::Data
                && f.source_root.is_none()
                && f.semantic.is_none(),
            "fixture XLSX requires raw DATA without a package root or semantic proof"
        );
    }
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
            && f.source_root
                .as_ref()
                .map_or(f.format == "numbers" || f.format == "xlsx", |root| root
                    .ends_with(&format!(".{extension}"))
                    && !root.contains(['/', '\\'])
                    && !root.chars().any(char::is_control)
                    && root.len() <= 255)
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
        let actual = if source && f.source_root.is_none() {
            cirrove_icloud::package_flat_archive_semantic_identity_v2(
                file,
                receipt,
                &CancellationToken::new(),
            )?
        } else {
            cirrove_icloud::package_archive_semantic_identity_versioned(
                file,
                receipt,
                if source {
                    f.source_root
                        .as_deref()
                        .context("fixture source root absent")?
                } else {
                    &f.expected_root
                },
                expected.version,
                &CancellationToken::new(),
            )?
        };
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

mod retained_numbers_read;
pub use retained_numbers_read::icloud_owned_retained_numbers_read;

mod editor_metadata;
pub use editor_metadata::icloud_owned_editor_metadata;

mod calc_metadata;
pub use calc_metadata::icloud_owned_calc_metadata;

mod editor_source;
pub use editor_source::icloud_owned_editor_source_proof;

mod native_import_fixture;
pub use native_import_fixture::icloud_owned_native_import_fixture_verify;

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
    fn owned_fixture_xlsx_data_accepts_exact_raw_registration() -> Result<()> {
        let (bytes, _) = fixture()?;
        let mut input: serde_json::Value = serde_json::from_slice(&bytes)?;
        input["format"] = "xlsx".into();
        input["document"]["extension"] = "xlsx".into();
        input["source"] = "/var/tmp/owned-source.xlsx".into();
        input["source_root"] = serde_json::Value::Null;
        input["expected_root"] =
            format!("{}.xlsx", input["document"]["name"].as_str().unwrap()).into();
        let registered = serde_json::to_vec(&input)?;
        let f = decode(&registered, &hex::encode(Sha256::digest(&registered)))?;
        assert_eq!(f.representation, FixtureRepresentation::Data);
        assert!(f.semantic.is_none() && f.source_root.is_none());
        validate(&f)?;
        Ok(())
    }
    #[test]
    fn owned_fixture_xlsx_refuses_package_semantic_wrapped_root_and_extension() -> Result<()> {
        let (_, mut f) = fixture()?;
        f.format = "xlsx".into();
        f.document.extension = "xlsx".into();
        f.source = "/var/tmp/owned-source.xlsx".into();
        f.source_root = None;
        f.expected_root = format!("{}.xlsx", f.document.name);
        let semantic = PackageSemanticIdentity {
            version: 2,
            sha256: "a".repeat(64),
            entries: 2,
            files: 1,
            expanded_bytes: 3,
        };
        semantic.validate()?;
        f.representation = FixtureRepresentation::Package;
        assert!(
            validate(&f).is_err(),
            "XLSX PACKAGE without semantic accepted"
        );
        f.semantic = Some(semantic);
        assert!(validate(&f).is_err(), "XLSX PACKAGE with semantic accepted");
        f.representation = FixtureRepresentation::Data;
        assert!(
            validate(&f).is_err(),
            "XLSX DATA with package semantic accepted"
        );
        f.semantic = None;
        f.source_root = Some("Export.xlsx".into());
        assert!(validate(&f).is_err(), "XLSX wrapped source root accepted");
        f.source_root = None;
        f.document.extension = "numbers".into();
        assert!(
            validate(&f).is_err(),
            "XLSX document extension mismatch accepted"
        );
        f.document.extension = "xlsx".into();
        f.expected_root = format!("{}.numbers", f.document.name);
        assert!(
            validate(&f).is_err(),
            "XLSX expected extension mismatch accepted"
        );
        Ok(())
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
            &format!("{}/Index/Document.iwa", f.source_root.as_deref().unwrap()),
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
            f.source_root.as_deref().unwrap(),
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
        f.source_root = Some("Wrong.pages".into());
        assert!(proof(&f, &a, &ar, true).is_err());
        f.expected_root = "Wrong.pages".into();
        assert!(proof(&f, &b, &br, false).is_err());
        Ok(())
    }

    #[test]
    fn owned_fixture_explicit_flat_source_matches_exact_wrapped_current_v2() -> Result<()> {
        let dir = tempfile::tempdir()?.keep();
        let (raw, _) = fixture()?;
        let mut input: serde_json::Value = serde_json::from_slice(&raw)?;
        input["format"] = "numbers".into();
        input["document"]["extension"] = "numbers".into();
        let expected = format!("{}.numbers", input["document"]["name"].as_str().unwrap());
        input["expected_root"] = expected.clone().into();
        input["representation"] = "package".into();
        input["source_root"] = serde_json::Value::Null;
        let flat = zip_archive("Index/Document.iwa", b"fixed content");
        let wrapped = zip_archive(&format!("{expected}/Index/Document.iwa"), b"fixed content");
        let a_path = dir.join("flat.numbers");
        let b_path = dir.join("wrapped.numbers");
        std::fs::write(&a_path, &flat)?;
        std::fs::write(&b_path, &wrapped)?;
        let a = File::open(&a_path)?;
        let b = File::open(&b_path)?;
        let ar = PackageDownload {
            size: flat.len() as u64,
            sha256: hex::encode(Sha256::digest(&flat)),
        };
        let br = PackageDownload {
            size: wrapped.len() as u64,
            sha256: hex::encode(Sha256::digest(&wrapped)),
        };
        input["source_size"] = ar.size.into();
        input["source_sha256"] = ar.sha256.clone().into();
        input["source"] = a_path.to_string_lossy().as_ref().into();
        input["semantic"] =
            serde_json::to_value(cirrove_icloud::package_archive_semantic_identity_versioned(
                &b,
                &br,
                &expected,
                2,
                &CancellationToken::new(),
            )?)?;
        let bytes = serde_json::to_vec(&input)?;
        let result = decode(&bytes, &hex::encode(Sha256::digest(&bytes)));
        assert_eq!(std::fs::read(&a_path)?, flat);
        assert_eq!(std::fs::read(&b_path)?, wrapped);
        let f = result.expect("flat source fixture rejected before actual content proof");
        validate(&f)?;
        proof(&f, &a, &ar, true)?;
        proof(&f, &b, &br, false)?;
        assert_ne!(ar.sha256, br.sha256);
        // Only the source may be flat; provider package read retains its exact root.
        assert!(proof(&f, &a, &ar, false).is_err());
        for (name, body) in [
            ("../Index/Document.iwa", b"fixed content".as_slice()),
            ("/Index/Document.iwa", b"fixed content".as_slice()),
            ("Index\\Document.iwa", b"fixed content".as_slice()),
            ("Index/Other.iwa", b"fixed content".as_slice()),
            ("Index/Document.iwa", b"changed bytes".as_slice()),
        ] {
            let bad = zip_archive(name, body);
            let bad_path = dir.join(format!("negative-{}", hex::encode(Sha256::digest(&bad))));
            std::fs::write(&bad_path, &bad)?;
            let bad_file = File::open(&bad_path)?;
            let bad_receipt = PackageDownload {
                size: bad.len() as u64,
                sha256: hex::encode(Sha256::digest(&bad)),
            };
            // Bind independent raw identity, then exercise flat parser/content proof.
            let mut bad_input = input.clone();
            bad_input["source_size"] = bad_receipt.size.into();
            bad_input["source_sha256"] = bad_receipt.sha256.clone().into();
            let raw = serde_json::to_vec(&bad_input)?;
            let bad_fixture = decode(&raw, &hex::encode(Sha256::digest(&raw)))?;
            assert!(
                proof(&bad_fixture, &bad_file, &bad_receipt, true).is_err(),
                "flat content/path guard"
            );
        }
        input.as_object_mut().unwrap().remove("source_root");
        let missing = serde_json::to_vec(&input)?;
        assert!(decode(&missing, &hex::encode(Sha256::digest(&missing))).is_err());
        input["source_root"] = serde_json::Value::Null;
        input["format"] = "pages".into();
        input["document"]["extension"] = "pages".into();
        input["expected_root"] =
            format!("{}.pages", input["document"]["name"].as_str().unwrap()).into();
        let bytes = serde_json::to_vec(&input)?;
        assert!(validate(&decode(&bytes, &hex::encode(Sha256::digest(&bytes)))?).is_err());
        Ok(())
    }
}
