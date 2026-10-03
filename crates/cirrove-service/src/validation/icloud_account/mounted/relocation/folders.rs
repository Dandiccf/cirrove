//! Continue the same owned mount with a populated folder and both name blockers.
use super::*;

pub(super) async fn paths(f: &Fixture) -> Result<()> {
    application(f,r#"
import pathlib,sys
p=pathlib.Path(sys.argv[1])
assert not (p/'Folder Source').exists()
assert (p/'Folder Target').is_dir()
assert (p/'Destination/Folder Source').is_dir()
assert (p/'Destination/Folder Target/Child.txt').read_bytes()==b'Cirrove account-router original\n'
assert not any(n.name.startswith('.cirrove-move-') for d in (p,p/'Destination') for n in d.iterdir())
"#).await
}
pub(super) async fn run(f: &Fixture, session: &WritableSession, destination: &Node) -> Result<()> {
    application(
        f,
        r#"
import pathlib,sys
p=pathlib.Path(sys.argv[1])
(p/'Folder Source').mkdir(); (p/'Folder Target').mkdir(); (p/'Destination/Folder Source').mkdir()
"#,
    )
    .await?;
    let rows = mutations(session, 6).await?;
    let original = upsert(&rows, &f.parent.id, "Folder Source")?;
    let source_blocker = upsert(&rows, &f.parent.id, "Folder Target")?;
    let destination_blocker = upsert(&rows, &destination.id, "Folder Source")?;
    application(
        f,
        r#"
import os,pathlib,sys
p=pathlib.Path(sys.argv[1])/'Folder Source/Child.txt'
with p.open('xb') as f:
    f.write(b'Cirrove account-router original\n'); f.flush(); os.fsync(f.fileno())
"#,
    )
    .await?;
    let child = uploaded(session, 3).await?;
    ensure!(
        child.parent_id.as_ref() == Some(&original.id) && child.name == "Child.txt",
        "unexpected child receipt"
    );
    hash(f, &child, FIRST).await?;
    application(
        f,
        r#"
import os,pathlib,sys
p=pathlib.Path(sys.argv[1]); child=(p/'Folder Source/Child.txt').open('rb')
os.rename(p/'Folder Source',p/'Destination/Folder Target')
assert child.read()==b'Cirrove account-router original\n'; child.close()
assert (p/'Destination/Folder Target/Child.txt').read_bytes()==b'Cirrove account-router original\n'
"#,
    )
    .await?;
    let rows = mutations(session, 7).await?;
    let moved = upsert(&rows, &destination.id, "Folder Target")?;
    ensure!(
        moved.id == original.id,
        "folder relocation changed identity"
    );
    hash(f, &child, FIRST).await?;
    let mut remote =
        ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
    let root = remote.list_folder(&f.parent.id).await?;
    let target = remote.list_folder(&destination.id).await?;
    ensure!(
        root.iter()
            .any(|n| n.drivewsid == source_blocker.id && n.display_name() == "Folder Target")
            && !root.iter().any(|n| n.drivewsid == original.id)
            && target
                .iter()
                .any(|n| n.drivewsid == destination_blocker.id
                    && n.display_name() == "Folder Source")
            && target
                .iter()
                .any(|n| n.drivewsid == original.id && n.display_name() == "Folder Target")
            && !root
                .iter()
                .chain(&target)
                .any(|n| n.display_name().starts_with(".cirrove-move-")),
        "combined folder location or blockers differ"
    );
    paths(f).await?;
    record(
        &f.run_dir.join("folder-verified.json"),
        &serde_json::json!({"moved":moved,"child":child,"source_blocker":source_blocker,"destination_blocker":destination_blocker}),
    )?;
    println!(
        "Populated folder moved and renamed through FUSE; child and blockers independently verified"
    );
    Ok(())
}
