//! Real-provider acceptance restricted to this run's generated files.
use super::*;
use crate::journal::{MutationRecord, MutationState};

const DOCUMENT: &str = "Grüße & Kärnten.txt";
const TEMPORARY: &str = ".Cirrove-atomär.tmp";
const ONLINE: &str = "Cirrove-online-Quelle.txt";

// No account path is accepted: the caller supplies its fresh fixture mount.
const APPLICATION: &str = r#"
import hashlib,json,os,sys,time
root,mode,generation=sys.argv[1],sys.argv[2],int(sys.argv[3])
def data(g):
    size={2:262177,3:196613,4:229379,5:294911}[g]
    return bytes((i*13+g*17)%251 for i in range(size))
body=data(generation)
target=os.path.join(root,'Grüße & Kärnten.txt')
source=os.path.join(root,'Cirrove-online-Quelle.txt' if mode!='local' else '.Cirrove-atomär.tmp')
old=None
if mode!='create':
    old=open(target,'rb',buffering=0)
    old_inode=os.fstat(old.fileno()).st_ino
start=time.monotonic()
if mode!='online':
    with open(source,'xb',buffering=0) as file:
        view=memoryview(body)
        while view:
            count=file.write(view)
            if not count: raise OSError('short write')
            view=view[count:]
        os.fsync(file.fileno())
source_inode=os.stat(source).st_ino
report=dict(generation=generation,size=len(body),sha256=hashlib.sha256(body).hexdigest(),source_inode=source_inode)
if mode!='create':
    rename_start=time.monotonic()
    os.replace(source,target)
    report['rename_ms']=(time.monotonic()-rename_start)*1000
    assert not os.path.exists(source)
    assert os.stat(target).st_ino==source_inode and source_inode!=old_inode
    assert os.fstat(old.fileno()).st_nlink==0
    assert old.read()==data(generation-1)
    old.close()
    with open(target,'rb') as current:
        assert current.read()==body
    report['old_descriptor_preserved']=True
report['application_ms']=(time.monotonic()-start)*1000
print(json.dumps(report))
"#;

async fn application(root: &Path, mode: &str, generation: u32) -> Result<serde_json::Value> {
    let output = tokio::process::Command::new("python3")
        .args(["-c", APPLICATION])
        .arg(root)
        .arg(mode)
        .arg(generation.to_string())
        .kill_on_drop(true)
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "atomic-save application failed; fixture and journal retained"
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn completed(
    session: &WritableSession,
    uploads: usize,
    mutations: usize,
) -> Result<(Vec<UploadRecord>, Vec<MutationRecord>)> {
    loop {
        let saves = session.uploads(0, 100).await?;
        let changes = session.mutations(0, 100).await?;
        anyhow::ensure!(
            saves.len() <= uploads && changes.len() <= mutations,
            "unexpected operation count; fixture and journal retained"
        );
        anyhow::ensure!(
            !saves
                .iter()
                .any(|r| matches!(r.state, UploadState::Failed | UploadState::Conflict))
                && !changes.iter().any(|r| matches!(
                    r.state,
                    MutationState::Failed | MutationState::Conflict | MutationState::NeedsReview
                )),
            "atomic-save cloud operation requires review; fixture and journal retained"
        );
        if saves.len() == uploads
            && changes.len() == mutations
            && saves.iter().all(|r| r.state == UploadState::Uploaded)
            && changes.iter().all(|r| r.state == MutationState::Applied)
        {
            return Ok((saves, changes));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn matching_bytes(record: &UploadRecord, report: &serde_json::Value) -> Result<()> {
    anyhow::ensure!(
        Some(record.size) == report["size"].as_u64()
            && Some(record.sha256.as_str()) == report["sha256"].as_str(),
        "sealed bytes differ from application report"
    );
    Ok(())
}

async fn verify_pair(
    provider: &FixtureGraph,
    source: &UploadRecord,
    replacement: &UploadRecord,
    mutation: &MutationRecord,
    previous: &UploadRecord,
    cancel: &CancellationToken,
) -> Result<()> {
    let source_node = source.remote.as_ref().context("source receipt missing")?;
    let target = previous.remote.as_ref().context("target receipt missing")?;
    anyhow::ensure!(
        matches!(&replacement.intent, UploadIntent::Replace { item, expected_etag }
            if item == &target.id && Some(expected_etag) == target.etag.as_ref())
            && replacement
                .remote
                .as_ref()
                .is_some_and(|n| n.id == target.id),
        "replacement did not keep its conditional destination identity"
    );
    anyhow::ensure!(
        matches!(&mutation.request.intent, MutationIntent::RemoveFile { before }
            if before.id == source_node.id && before.etag == source_node.etag)
            && mutation
                .receipt
                .as_ref()
                .is_some_and(|r| mutation.request.accepts(r)),
        "cleanup did not confirm the original source identity and version"
    );
    verify(provider, replacement, cancel).await?;
    anyhow::ensure!(
        matches!(
            provider
                .node(&provider.scope, &source_node.id, cancel)
                .await,
            Err(ProviderError::NotFound)
        ),
        "confirmed source cleanup is not reflected by independent lookup"
    );
    Ok(())
}

pub(super) struct OnlinePair {
    target: UploadRecord,
    source: UploadRecord,
}

async fn sources_retired(journal: &Arc<Mutex<UploadJournal>>, pair: &OnlinePair) -> Result<bool> {
    let shared = journal.clone();
    let scope = pair.source.scope.clone();
    let ids = [&pair.source, &pair.target]
        .into_iter()
        .map(|r| {
            r.remote
                .as_ref()
                .map(|n| n.id.clone())
                .context("missing receipt")
        })
        .collect::<Result<Vec<_>>>()?;
    tokio::task::spawn_blocking(move || {
        let j = shared
            .lock()
            .map_err(|_| anyhow::anyhow!("journal unavailable"))?;
        for id in ids {
            let object = j
                .namespace_by_remote(&scope, &id)?
                .context("missing local identity")?;
            if object.working_file.is_some() || !object.follows_remote {
                return Ok(false);
            }
        }
        Ok(true)
    })
    .await?
}

pub(super) async fn local_saves(
    session: &WritableSession,
    journal: &Arc<Mutex<UploadJournal>>,
    engine: &Engine,
    provider: &FixtureGraph,
    log: &mut File,
    mut previous: UploadRecord,
    cancel: &CancellationToken,
) -> Result<OnlinePair> {
    // Counts are absolute upload-receipt totals, so they carry the three direct
    // application saves that precede this sequence.
    for (generation, count) in [(3, 5), (4, 7)] {
        let started = std::time::Instant::now();
        let report = application(&engine.account.mount_path, "local", generation).await?;
        event(
            log,
            serde_json::json!({"stage":"atomic_local_save","application":report}),
        )?;
        let (uploads, mutations) = completed(session, count, (generation - 2) as usize).await?;
        let source = &uploads[count - 2];
        let replacement = &uploads[count - 1];
        anyhow::ensure!(
            matches!(&source.intent, UploadIntent::Create { name, parent }
                if name == TEMPORARY && parent == &provider.root.id),
            "unexpected atomic source"
        );
        matching_bytes(source, &report)?;
        matching_bytes(replacement, &report)?;
        verify_pair(
            provider,
            source,
            replacement,
            &mutations[mutations.len() - 1],
            &previous,
            cancel,
        )
        .await?;
        event(
            log,
            serde_json::json!({"stage":"atomic_cloud_verified","generation":generation,
            "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"application":report,
            "replacement":replacement,"cleanup":mutations.last()}),
        )?;
        previous = replacement.clone();
    }
    let report = application(&engine.account.mount_path, "create", 5).await?;
    let (uploads, _) = completed(session, 8, 2).await?;
    let source = uploads.last().context("online source missing")?.clone();
    anyhow::ensure!(
        matches!(&source.intent, UploadIntent::Create { name, parent }
            if name == ONLINE && parent == &provider.root.id),
        "unexpected online source"
    );
    matching_bytes(&source, &report)?;
    verify(provider, &source, cancel).await?;
    let pair = OnlinePair {
        target: previous,
        source,
    };
    while !sources_retired(journal, &pair).await? {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    event(
        log,
        serde_json::json!({"stage":"online_source_retired","application":report,
        "source":pair.source,"target":pair.target}),
    )?;
    Ok(pair)
}

pub(super) async fn after_remount(
    session: &WritableSession,
    journal: &Arc<Mutex<UploadJournal>>,
    engine: &Engine,
    provider: &FixtureGraph,
    log: &mut File,
    pair: OnlinePair,
    cancel: &CancellationToken,
) -> Result<()> {
    anyhow::ensure!(
        sources_retired(journal, &pair).await?,
        "remount source is not online-only"
    );
    let started = std::time::Instant::now();
    let report = application(&engine.account.mount_path, "online", 5).await?;
    event(
        log,
        serde_json::json!({"stage":"online_atomic_save","application":report}),
    )?;
    let (uploads, mutations) = completed(session, 9, 3).await?;
    let latest = uploads.last().context("replacement missing")?;
    matching_bytes(latest, &report)?;
    verify_pair(
        provider,
        &pair.source,
        latest,
        &mutations[2],
        &pair.target,
        cancel,
    )
    .await?;
    // Read the entire known fixture root, including unexpected foreign entries;
    // the mounted wrapper intentionally filters unknown IDs from its view.
    let listing = provider
        .graph
        .children(&provider.scope, &provider.root.id, None, cancel)
        .await?;
    anyhow::ensure!(
        listing.next.is_none() && listing.nodes.len() == 1 && listing.nodes[0].name == DOCUMENT,
        "unexpected final cloud fixture listing"
    );
    event(
        log,
        serde_json::json!({"stage":"online_atomic_cloud_verified",
        "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"application":report,
        "replacement":latest,"cleanup":mutations[2]}),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn separate_atomic_save_application_preserves_streams_and_reuses_temporary_name() {
        let temp = tempfile::tempdir().expect("fixture");
        let bytes: Vec<u8> = (0..262177).map(|i| ((i * 13 + 34) % 251) as u8).collect();
        std::fs::write(temp.path().join(DOCUMENT), bytes).expect("initial file");
        for generation in [3, 4] {
            let report = application(temp.path(), "local", generation)
                .await
                .expect("atomic save");
            assert_eq!(report["old_descriptor_preserved"], true);
            assert!(!temp.path().join(TEMPORARY).exists());
        }
        application(temp.path(), "create", 5).await.expect("source");
        let report = application(temp.path(), "online", 5)
            .await
            .expect("online replacement");
        assert_eq!(report["old_descriptor_preserved"], true);
        assert!(!temp.path().join(ONLINE).exists());
        let bytes = std::fs::read(temp.path().join(DOCUMENT)).expect("final bytes");
        assert_eq!(report["sha256"], hex::encode(Sha256::digest(bytes)));
    }
}
