//! Beyond both historical 32 MiB adapter and 64 MiB manager limits.
use super::*;
const ORIGINAL_SIZE: u64 = 65 * 1024 * 1024 + 17;
const REPLACEMENT_SIZE: u64 = 66 * 1024 * 1024 + 29;
const BUDGET: u64 = 512 * 1024 * 1024;

const APP: &str = r#"
import os,sys
path=os.path.join(sys.argv[1], 'Account Router.txt')
mode,size,value=sys.argv[2],int(sys.argv[3]),int(sys.argv[4])
block=bytes([value])*65536
if mode != 'read':
    flags=os.O_WRONLY | (os.O_CREAT | os.O_EXCL if mode == 'create' else os.O_TRUNC)
    fd=os.open(path,flags,0o600)
    try:
        remaining=size
        while remaining:
            written=os.write(fd,block[:min(len(block),remaining)])
            assert written>0
            remaining-=written
        os.fsync(fd)
    finally:
        os.close(fd)
else:
    received=0
    with open(path,'rb',buffering=0) as f:
        while True:
            data=f.read(len(block))
            if not data: break
            assert data==block[:len(data)]
            received+=len(data)
    assert received==size
assert os.stat(path).st_size==size
"#;

async fn app(f: &Fixture, mode: &str, size: u64, value: u8) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(600),
        tokio::process::Command::new("python3")
            .arg("-c")
            .arg(APP)
            .arg(f.run_dir.join("mount"))
            .arg(mode)
            .arg(size.to_string())
            .arg(value.to_string())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        output.status.success(),
        "large mounted application failed; state retained"
    );
    Ok(())
}

async fn verify(f: &Fixture, node: &Node, size: u64, value: u8) -> Result<()> {
    ensure!(
        node.size == size
            && node.parent_id.as_ref() == Some(&f.parent.id)
            && node.name == NAME
            && node.kind == NodeKind::File
            && !node.package
            && node.target.is_none(),
        "large receipt differs from the owned fixture"
    );
    let mut expected = Sha256::new();
    let block = [value; 65536];
    let mut left = size;
    while left > 0 {
        let n = left.min(block.len() as u64) as usize;
        expected.update(&block[..n]);
        left -= n as u64;
    }
    let mut remote =
        ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
    let actual = remote
        .hash_file_in_folder_for_revision(
            &f.parent.id,
            &node.id,
            node.etag
                .as_deref()
                .context("large receipt lacks revision")?,
            size,
        )
        .await?;
    ensure!(
        actual == hex::encode(expected.finalize()),
        "large independent digest mismatch"
    );
    Ok(())
}

pub async fn icloud_account_mounted_large(run: Uuid) -> Result<()> {
    run_large(run, ORIGINAL_SIZE, REPLACEMENT_SIZE, BUDGET).await
}

fn sized_plan(mib: u64) -> Result<(u64, u64, u64)> {
    ensure!(
        (65..=2048).contains(&mib),
        "large fixture size must be 65..2048 MiB"
    );
    Ok((
        mib * 1024 * 1024 + 17,
        (mib + 1) * 1024 * 1024 + 29,
        mib * 8 * 1024 * 1024,
    ))
}

pub async fn icloud_account_mounted_large_sized(run: Uuid, mib: u64) -> Result<()> {
    let (original, replacement, budget) = sized_plan(mib)?;
    run_large(run, original, replacement, budget).await
}

async fn run_large(
    run: Uuid,
    original_size: u64,
    replacement_size: u64,
    budget: u64,
) -> Result<()> {
    let f = prepare_with_budget(run, "mounted-large", budget).await?;
    let session = mount(&f).await?;
    let result: Result<()> = async {
        app(&f, "create", original_size, 0x35).await?;
        let original = uploaded(&session, 1).await?;
        verify(&f, &original, original_size, 0x35).await?;
        record(&f.run_dir.join("original.json"), &original)?;
        println!("Large mounted file: create and independent digest verified");
        app(&f, "replace", replacement_size, 0x6a).await?;
        let replacement = uploaded(&session, 2).await?;
        verify(&f, &replacement, replacement_size, 0x6a).await?;
        let mut remote =
            ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
        ensure!(
            replacement.id != original.id && remote.exact_item_in_trash(&original.id).await?,
            "large replacement recovery identity not verified"
        );
        record(&f.run_dir.join("replacement.json"), &replacement)?;
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    let reopened = mount(&f).await?;
    let read = app(&f, "read", replacement_size, 0x6a).await;
    let shutdown = reopened.shutdown().await;
    read?;
    shutdown?;
    record(
        &f.run_dir.join("passed.json"),
        &serde_json::json!({"run":run,
        "original_size":original_size,"replacement_size":replacement_size,"journal_budget":budget,
        "mounted_create":true,"mounted_replace":true,"independent_digests":true,
        "original_in_trash":true,"remounted_full_read":true}),
    )?;
    println!("Large mounted file: replacement, recovery and full remounted read verified");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sized_large_fixture_is_bounded_before_any_cloud_setup() {
        for invalid in [0, 64, 2049, u64::MAX] {
            assert!(sized_plan(invalid).is_err());
        }
        assert_eq!(
            sized_plan(1024).expect("one GiB"),
            (1_073_741_841, 1_074_790_429, 8_589_934_592)
        );
        assert!(sized_plan(2048).is_ok());
    }
}
