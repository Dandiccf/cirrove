//! Feature-only offline source scan; no account, state, session or provider API.
use super::{LIMIT, hex_digest, open_private, same_directory};
use anyhow::{Result, ensure};
use cirrove_core::{CancellationToken, upload::PackageSemanticIdentity};
use cirrove_icloud::PackageDownload;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    path: PathBuf,
    size: u64,
    sha256: String,
    root: String,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u32,
    run: Uuid,
    phase: String,
    session_directory: PathBuf,
    root_dev: u64,
    root_ino: u64,
    source: Source,
}
fn private_bytes(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_private(path, false, 16 * 1024)?
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024,
        "editor source registration bound refused"
    );
    Ok(bytes)
}
fn registered(bytes: &[u8], digest: &str) -> Result<Registration> {
    ensure!(
        hex_digest(digest)
            && bytes.len() <= 16 * 1024
            && hex::encode(Sha256::digest(bytes)) == digest,
        "editor source registration changed"
    );
    let r: Registration = serde_json::from_slice(bytes)?;
    ensure!(
        r.version == 1
            && !r.run.is_nil()
            && matches!(r.phase.as_str(), "a" | "b")
            && r.session_directory.as_path()
                == Path::new(&format!(
                    "/var/tmp/cirrove-numbers-browser-editor-{}",
                    r.run
                ))
            && r.root_dev > 0
            && r.root_ino > 0
            && r.source.path
                == r.session_directory
                    .join(format!("source-{}.numbers", r.phase))
            && r.source.size > 0
            && r.source.size <= LIMIT
            && hex_digest(&r.source.sha256)
            && r.source.root.ends_with(".numbers")
            && r.source.root.len() <= 255
            && !r.source.root.contains(['/', '\\'])
            && !r.source.root.chars().any(char::is_control),
        "editor source registration scope refused"
    );
    Ok(r)
}
fn source_file(source: &Source) -> Result<File> {
    let file = open_private(&source.path, false, LIMIT)?;
    ensure!(
        file.metadata()?.permissions().mode() & 0o777 == 0o400,
        "editor source mode refused"
    );
    Ok(file)
}
fn raw_digest(file: &mut File, expected_size: u64) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        ensure!(size <= expected_size, "editor source raw bound changed");
        digest.update(&buffer[..count]);
    }
    ensure!(size == expected_size, "editor source raw length changed");
    Ok(hex::encode(digest.finalize()))
}
fn scan(source: &Source) -> Result<PackageSemanticIdentity> {
    let mut held = source_file(source)?;
    let before = held.metadata()?;
    ensure!(
        before.len() == source.size && raw_digest(&mut held, source.size)? == source.sha256,
        "editor source raw identity differs"
    );
    let receipt = PackageDownload {
        size: source.size,
        sha256: source.sha256.clone(),
    };
    let semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
        &held,
        &receipt,
        &source.root,
        2,
        &CancellationToken::new(),
    )?;
    semantic.validate()?;
    ensure!(
        semantic.version == 2,
        "editor source semantic version differs"
    );
    let mut current = source_file(source)?;
    let after = current.metadata()?;
    ensure!(
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && after.len() == source.size
            && held.metadata()?.permissions().mode() & 0o777 == 0o400
            && raw_digest(&mut held, source.size)? == source.sha256
            && raw_digest(&mut current, source.size)? == source.sha256,
        "editor source changed during scan"
    );
    Ok(semantic)
}
fn observe(path: &Path, digest: &str) -> Result<serde_json::Value> {
    let r = registered(&private_bytes(path)?, digest)?;
    ensure!(
        path == r
            .session_directory
            .join(format!("editor-{}-source-registration.json", r.phase)),
        "editor source registration path refused"
    );
    let root = open_private(&r.session_directory, true, 0)?;
    let meta = root.metadata()?;
    ensure!(
        meta.dev() == r.root_dev && meta.ino() == r.root_ino,
        "editor source root changed"
    );
    let semantic = scan(&r.source)?;
    let _ = registered(&private_bytes(path)?, digest)?;
    same_directory(&r.session_directory, &root)?;
    Ok(
        serde_json::json!({"version":1,"run":r.run,"phase":r.phase,"registration_sha256":digest,
        "source":{"path":r.source.path,"size":r.source.size,"sha256":r.source.sha256,
            "root":r.source.root,"semantic":semantic},"semantic_version":2,
        "raw_source_identity_verified":true,"package_semantic_identity_verified":true,
        "provider_representation_verified":false,"current_content_verified":false,
        "gui_fidelity_verified":false,"offline_only":true,"cloud_mutated":false}),
    )
}
/// Computes a new identity from the registered actual archive; never copies A.
pub fn icloud_owned_editor_source_proof(path: &Path, digest: &str) -> Result<serde_json::Value> {
    observe(path, digest).map_err(|_| anyhow::anyhow!("owned iCloud editor source proof refused"))
}

#[cfg(test)]
mod tests;
