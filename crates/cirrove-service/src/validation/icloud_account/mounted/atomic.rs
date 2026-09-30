//! Editor-style saves through real FUSE and the normal iCloud account router.
use super::*;
use crate::journal::MutationState;

const EDITOR: &str = r#"
import os,sys
os.chdir(sys.argv[1])
old=os.open('Account Router.txt',os.O_RDONLY)
first=os.open('.cirrove-editor-one',os.O_CREAT|os.O_EXCL|os.O_RDWR,0o600)
second=None
try:
    assert os.read(old,100)==b'Cirrove account-router original\n'
    one=b'Cirrove first atomic save\n'
    assert os.write(first,one)==len(one)
    os.fsync(first)
    inode=os.fstat(first).st_ino
    os.replace('.cirrove-editor-one','Account Router.txt')
    assert os.stat('Account Router.txt').st_ino==inode
    assert os.fstat(old).st_nlink==0
    assert os.pread(old,100,0)==b'Cirrove account-router original\n'
    assert open('Account Router.txt','rb').read()==one
    second=os.open('.cirrove-editor-two',os.O_CREAT|os.O_EXCL|os.O_RDWR,0o600)
    two=b'Cirrove account-router replacement\n'
    assert os.write(second,two)==len(two)
    os.fsync(second)
    os.replace('.cirrove-editor-two','Account Router.txt')
    assert os.fstat(first).st_nlink==0
    assert os.pread(first,100,0)==one
    assert os.pread(old,100,0)==b'Cirrove account-router original\n'
    assert open('Account Router.txt','rb').read()==two
    assert os.listdir('.')==['Account Router.txt']
finally:
    os.close(old)
    os.close(first)
    if second is not None: os.close(second)
"#;

async fn cleanups(session: &WritableSession) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let rows = session.mutations(0, 16).await?;
            ensure!(rows.len() == 2, "unexpected editor cleanup count");
            ensure!(
                !rows.iter().any(|row| matches!(
                    row.state,
                    MutationState::Failed | MutationState::Conflict | MutationState::NeedsReview
                )),
                "editor cleanup requires review; state retained"
            );
            if rows.iter().all(|row| row.state == MutationState::Applied) {
                ensure!(
                    session.namespace_conflicts()?.is_empty(),
                    "editor namespace conflict"
                );
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .context("editor cleanup deadline; state retained")?
}

pub async fn icloud_account_mounted_atomic(run: Uuid) -> Result<()> {
    let f = prepare(run, "mounted-atomic").await?;
    let session = mount(&f).await?;
    let result: Result<()> = async {
        app(&f, "create").await?;
        let original = uploaded(&session, 1).await?;
        verify(&f.snapshot, &f.account, &f.parent, &original, FIRST).await?;
        record(&f.run_dir.join("original.json"), &original)?;
        println!("Editor arm: original upload and independent digest verified");
        let output = tokio::time::timeout(Duration::from_secs(120),
            tokio::process::Command::new("python3").arg("-c").arg(EDITOR)
                .arg(f.run_dir.join("mount")).kill_on_drop(true).output()).await
            .context("editor application deadline; state retained")??;
        ensure!(output.status.success(), "editor atomic saves failed; state retained");
        record(&f.run_dir.join("editor-local.json"), &serde_json::json!({
            "two_atomic_saves":true,"retained_descriptors":true,"one_visible_file":true}))?;
        println!("Editor arm: two atomic saves and retained descriptors verified locally");
        let current = uploaded(&session, 5).await?;
        cleanups(&session).await?;
        verify(&f.snapshot, &f.account, &f.parent, &current, SECOND).await?;
        let mut remote = ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
        let entries = remote.list_folder(&f.parent.id).await?;
        ensure!(entries.len() == 1 && entries[0].drivewsid == current.id,
            "editor temporary cloud files remain");
        let rows = session.uploads(0, 16).await?;
        let mut predecessors = std::collections::HashSet::new();
        for row in rows {
            let node = row.remote.context("editor upload missing receipt")?;
            if node.id != current.id {
                ensure!(predecessors.insert(node.id.clone()) && remote.exact_item_in_trash(&node.id).await?,
                    "editor predecessor is not recoverable");
            }
        }
        ensure!(predecessors.len() == 4 && predecessors.contains(&original.id),
            "editor recovery identity count differs");
        record(&f.run_dir.join("current.json"), &current)?;
        println!("Editor arm: five uploads, two cleanups, final digest and four Trash identities verified");
        Ok(())
    }.await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    let reopened = mount(&f).await?;
    let result: Result<()> = async {
        app(&f, "read").await?;
        ensure!(
            reopened.uploads(0, 16).await?.len() == 5,
            "reopened editor uploads differ"
        );
        cleanups(&reopened).await?;
        Ok(())
    }
    .await;
    let shutdown = reopened.shutdown().await;
    result?;
    shutdown?;
    record(
        &f.run_dir.join("passed.json"),
        &serde_json::json!({
        "run":run,"atomic_saves":2,"retained_descriptors":true,"confirmed_uploads":5,
        "confirmed_cleanups":2,"trash_identities":4,"final_digest_verified":true,"remounted_read":true}),
    )?;
    println!("Editor arm: remounted final read verified");
    Ok(())
}
