//! Atomic conflict arm: every changed item was created in this run's owned folder.
use super::*;
use crate::journal::MutationState;

const TEMP: &str = ".cirrove-conflicting-editor";
pub(super) async fn replace(f: &Fixture, session: &WritableSession) -> Result<()> {
    let result = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::process::Command::new("python3")
            .arg("-c")
            .arg(
                r#"
import os,pathlib,sys
p=pathlib.Path(sys.argv[1],sys.argv[2]); data=bytes.fromhex(sys.argv[3])
with p.open('xb') as f:
    f.write(data); f.flush(); os.fsync(f.fileno())
assert p.read_bytes()==data
"#,
            )
            .arg(f.run_dir.join("mount"))
            .arg(TEMP)
            .arg(hex::encode(SECOND))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("editor temporary creation deadline")??;
    ensure!(result.status.success(), "editor temporary creation failed");
    let temporary = uploaded(session, 2).await?;
    ensure!(
        temporary.name == TEMP && temporary.parent_id.as_ref() == Some(&f.parent.id),
        "temporary receipt differs"
    );
    let mut remote =
        ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
    let digest = remote
        .hash_file_in_folder_for_revision(
            &f.parent.id,
            &temporary.id,
            temporary
                .etag
                .as_deref()
                .context("temporary revision missing")?,
            temporary.size,
        )
        .await?;
    ensure!(
        digest == hex::encode(Sha256::digest(SECOND)),
        "temporary bytes differ"
    );
    record(&f.run_dir.join("atomic-source.json"), &temporary)?;
    let result = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::process::Command::new("python3")
            .arg("-c")
            .arg(
                r#"
import os,sys
os.chdir(sys.argv[1]); old=os.open('Account Router.txt',os.O_RDONLY)
try:
    assert os.read(old,100)==b'Cirrove account-router original\n'
    os.replace(sys.argv[2],'Account Router.txt')
    assert os.pread(old,100,0)==b'Cirrove account-router original\n'
    assert open('Account Router.txt','rb').read()==bytes.fromhex(sys.argv[3])
finally: os.close(old)
"#,
            )
            .arg(f.run_dir.join("mount"))
            .arg(TEMP)
            .arg(hex::encode(SECOND))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("editor atomic replacement deadline")??;
    ensure!(result.status.success(), "editor atomic replacement failed");
    println!("Atomic save accepted; confirmed temporary identity and retained original descriptor");
    Ok(())
}

pub(super) async fn cleanup(f: &Fixture, session: &WritableSession, refused: Uuid) -> Result<()> {
    let temporary: Node =
        serde_json::from_slice(&std::fs::read(f.run_dir.join("atomic-source.json"))?)?;
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let rows = session.mutations(0, 16).await?;
            ensure!(rows.len() == 2, "unexpected atomic rescue cleanup count");
            ensure!(
                rows[0].state == MutationState::Resolved && rows[0].receipt.is_none(),
                "old cleanup was not superseded"
            );
            ensure!(
                rows.iter().all(|r| r
                    .request
                    .intent
                    .before()
                    .is_some_and(|n| n.id == temporary.id)),
                "cleanup targeted another identity"
            );
            ensure!(
                !matches!(
                    rows[1].state,
                    MutationState::Failed | MutationState::Conflict | MutationState::NeedsReview
                ),
                "atomic rescue cleanup requires review"
            );
            if rows[1].state == MutationState::Applied {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("atomic rescue cleanup deadline")??;
    let mut remote =
        ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
    ensure!(
        remote.exact_item_in_trash(&temporary.id).await?,
        "temporary identity was not recoverable in Trash"
    );
    record(
        &f.run_dir.join("atomic-cleanup-verified.json"),
        &serde_json::json!({"refused":refused,"temporary_recoverable":true,"old_cleanup_resolved_without_receipt":true,"new_cleanup_applied":true}),
    )?;
    println!("Only the confirmed editor temporary identity was cleaned up, recoverably in Trash");
    Ok(())
}
