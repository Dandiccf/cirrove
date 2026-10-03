//! A second atomic save while the first is refused; fresh run-owned files only.
use super::*;
const TEMP: &str = ".cirrove-conflicting-editor-2";
pub(super) async fn replace_again(f: &Fixture, session: &WritableSession) -> Result<()> {
    application(f, false).await?;
    let temporary = tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let rows = session.uploads(0, 16).await?;
            ensure!(rows.len() == 4, "unexpected second-temporary queue");
            let row = rows.last().context("missing second temporary")?;
            ensure!(
                matches!(&row.intent,UploadIntent::Create{name,..} if name==TEMP),
                "wrong temporary intent"
            );
            ensure!(
                !matches!(row.state, UploadState::Failed | UploadState::Conflict),
                "second temporary refused"
            );
            if row.state == UploadState::Uploaded {
                break row.remote.clone().context("temporary receipt missing");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .context("second temporary deadline")??;
    ensure!(
        temporary.name == TEMP && temporary.parent_id.as_ref() == Some(&f.parent.id),
        "wrong temporary identity"
    );
    let mut remote =
        ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
    let digest = remote
        .hash_file_in_folder_for_revision(
            &f.parent.id,
            &temporary.id,
            temporary.etag.as_deref().context("missing revision")?,
            temporary.size,
        )
        .await?;
    ensure!(
        digest == hex::encode(Sha256::digest(AUTOSAVE_LAST)),
        "second temporary content differs"
    );
    record(&f.run_dir.join("atomic-source-2.json"), &temporary)?;
    application(f, true).await?;
    let rows = session.uploads(0, 16).await?;
    ensure!(
        rows.len() == 5
            && rows[2].state == UploadState::Conflict
            && rows[4].state == UploadState::Pending
            && rows[4].session_key.is_none()
            && rows[4].sha256 == hex::encode(Sha256::digest(AUTOSAVE_LAST)),
        "second replacement did not remain untouched behind the conflict"
    );
    record(
        &f.run_dir.join("atomic-chain-queued.json"),
        &serde_json::json!({"first":rows[2].id,"second":rows[4].id,"pending_atomic_successor":true}),
    )?;
    Ok(())
}
async fn application(f: &Fixture, replace: bool) -> Result<()> {
    let output = tokio::process::Command::new("python3")
        .arg("-c")
        .arg(
            r#"
import os,pathlib,sys
p=pathlib.Path(sys.argv[1]); temp=p/sys.argv[2]; data=bytes.fromhex(sys.argv[3])
if sys.argv[4]=='create':
    with temp.open('xb') as f:
        f.write(data); f.flush(); os.fsync(f.fileno())
    assert temp.read_bytes()==data
else:
    with (p/'Account Router.txt').open('rb') as held:
        old=held.read(); assert old==bytes.fromhex(sys.argv[5])
        os.replace(temp,p/'Account Router.txt')
        held.seek(0); assert held.read()==old
        assert (p/'Account Router.txt').read_bytes()==data
"#,
        )
        .arg(f.run_dir.join("mount"))
        .arg(TEMP)
        .arg(hex::encode(AUTOSAVE_LAST))
        .arg(if replace { "replace" } else { "create" })
        .arg(hex::encode(SECOND))
        .kill_on_drop(true)
        .output();
    ensure!(
        tokio::time::timeout(Duration::from_secs(60), output)
            .await
            .context("second atomic application deadline")??
            .status
            .success(),
        "second atomic application failed"
    );
    Ok(())
}
