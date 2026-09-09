//! Developer fixture for the two ways a mount learns about a change nobody told
//! it about: catch-up when a notification channel reconnects, and the ordinary
//! periodic refresh. Both exist in the code and neither had live evidence.
//!
//! The whole design is about attribution. A change discovered by a mount running
//! both mechanisms proves nothing about either, so each phase disables the other:
//! the catch-up phase sets a poll interval far longer than the run can last, and
//! the polling phase never connects a subscription at all. Before opening the
//! gate, each phase also asserts the change is *not* visible, so a discovery that
//! had already happened cannot be mistaken for one the mechanism produced.
use super::*;
use crate::{engine::Engine, filesystem::CloudFs};
use cirrove_core::{
    ChangePage, Cursor, DirectoryPage, MetadataProvider, ProviderError, ReadProvider,
    notifications::{ChangeHintSender, WatchEnd},
};
use std::{ffi::OsString, sync::atomic::AtomicU64};
use tokio::sync::watch;

/// Delegates every request to the real adapter. The only difference is that its
/// notification subscription waits for the gate to open. A closed gate is a
/// provider that cannot reach its notification endpoint, which is what a
/// disconnected mount looks like -- not one that reports itself connected.
struct Gated {
    graph: Arc<OneDrive>,
    gate: watch::Receiver<bool>,
    connects: AtomicU64,
    /// Delta requests. Both mechanisms under test end in one of these, and
    /// nothing else does, which is what makes a discovery attributable.
    deltas: AtomicU64,
    /// Directory listings. An actively viewed directory revalidates through
    /// these, which is a third way a change can surface and the reason this
    /// fixture must not watch the mount while it is waiting.
    listings: AtomicU64,
}
#[async_trait::async_trait]
impl MetadataProvider for Gated {
    fn provider_id(&self) -> &'static str {
        "onedrive"
    }
    async fn changes(
        &self,
        scope: &Scope,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        self.deltas.fetch_add(1, Ordering::SeqCst);
        self.graph.changes(scope, cursor, cancel).await
    }
    async fn watch_changes(
        &self,
        scope: &Scope,
        hints: ChangeHintSender,
        cancel: &CancellationToken,
    ) -> std::result::Result<WatchEnd, ProviderError> {
        let mut gate = self.gate.clone();
        while !*gate.borrow_and_update() {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(WatchEnd::Renew),
                changed = gate.changed() => {
                    if changed.is_err() {
                        return Ok(WatchEnd::Renew);
                    }
                }
            }
        }
        self.connects.fetch_add(1, Ordering::SeqCst);
        self.graph.watch_changes(scope, hints, cancel).await
    }
}
#[async_trait::async_trait]
impl ReadProvider for Gated {
    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        cancel: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        self.graph.node(scope, id, cancel).await
    }
    async fn children(
        &self,
        scope: &Scope,
        parent: &str,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        self.listings.fetch_add(1, Ordering::SeqCst);
        self.graph.children(scope, parent, cursor, cancel).await
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        // This check observes directory entries only. Refusing content keeps a
        // stray thumbnail request from downloading anything during the run.
        Err(ProviderError::Permission)
    }
}
async fn mounted_names(mount: &Path) -> Result<Vec<OsString>> {
    let mount = mount.to_owned();
    Ok(tokio::task::spawn_blocking(move || {
        std::fs::read_dir(mount)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<Vec<_>>>()
    })
    .await??)
}
/// Wait for `name` to appear, returning how long it took.
async fn await_entry(mount: &Path, name: &str, deadline: Duration) -> Result<Duration> {
    let started = tokio::time::Instant::now();
    loop {
        if mounted_names(mount)
            .await?
            .iter()
            .any(|entry| entry == OsString::from(name).as_os_str())
        {
            return Ok(started.elapsed());
        }
        anyhow::ensure!(
            started.elapsed() < deadline,
            "{name} never became visible within {deadline:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
/// Hold without touching the mount, asserting no delta ran.
///
/// The first version of this waited by listing the mount every 200 ms, and the
/// change appeared during the hold with nothing connected and the poll an hour
/// away. Reading a directory is not a passive act here: an actively viewed
/// directory revalidates, which is the behaviour `freshness.rs` exists to
/// measure. The observation produced what it was meant to rule out. So this
/// waits in silence and asserts on the mechanism instead of on visibility.
async fn hold_without_watching(provider: &Gated, window: Duration) -> Result<()> {
    let deltas = provider.deltas.load(Ordering::SeqCst);
    let listings = provider.listings.load(Ordering::SeqCst);
    tokio::time::sleep(window).await;
    // Both are checked. A delta would be the mechanism firing early; a listing
    // would be the directory still revalidating from the baseline read, which is
    // the third path that already invalidated one version of this fixture.
    let ran_deltas = provider.deltas.load(Ordering::SeqCst) - deltas;
    let ran_listings = provider.listings.load(Ordering::SeqCst) - listings;
    anyhow::ensure!(
        ran_deltas == 0 && ran_listings == 0,
        "{ran_deltas} delta requests and {ran_listings} directory listings ran while the \
         phase was meant to be idle; a discovery after this point would not be attributable"
    );
    Ok(())
}
/// Wait for the delta request that the enabled mechanism triggers.
async fn await_delta(provider: &Gated, from: u64, deadline: Duration) -> Result<Duration> {
    let started = tokio::time::Instant::now();
    loop {
        if provider.deltas.load(Ordering::SeqCst) > from {
            return Ok(started.elapsed());
        }
        anyhow::ensure!(
            started.elapsed() < deadline,
            "no delta request within {deadline:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
struct PhaseResult {
    delta_ms: u128,
    visible_ms: u128,
    connects: u64,
    listings_before_delta: u64,
}
/// One phase: index a folder, change it behind the mount's back, then enable
/// exactly one discovery mechanism and time how long the change takes to appear.
#[allow(clippy::too_many_arguments)]
async fn phase(
    directory: &Path,
    account: &accounts::Account,
    graph: Arc<OneDrive>,
    scope: &Scope,
    root: &Node,
    log: &mut File,
    phase_name: &str,
    child: &str,
    poll_seconds: u64,
    open_gate_after_change: bool,
    hidden_window: Duration,
    deadline: Duration,
) -> Result<PhaseResult> {
    let (gate, gate_rx) = watch::channel(false);
    let provider = Arc::new(Gated {
        graph: graph.clone(),
        gate: gate_rx,
        connects: AtomicU64::new(0),
        deltas: AtomicU64::new(0),
        listings: AtomicU64::new(0),
    });
    let mut config = account.clone();
    config.root_id = root.id.clone();
    config.mount_path = directory.join(format!("mount-{phase_name}"));
    config.poll_seconds = poll_seconds;
    crate::private_dir(&config.mount_path)?;
    let engine = Engine::new(
        config,
        provider.clone(),
        directory.join(format!("engine-{phase_name}")),
    )
    .await?;
    let cancel = CancellationToken::new();
    let mut session = None;
    let mount = engine.account.mount_path.clone();
    let result = async {
        engine.start().await?;
        while !engine.health().await.iter().any(|h| h.state == "ready") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let fs = CloudFs::new(engine.clone())?;
        let target = mount.clone();
        session = Some(tokio::task::spawn_blocking(move || fs.mount(&target)).await??);
        // Settle the first listing before changing anything, so that a later
        // appearance is a refresh finding the change and not the initial index
        // happening to run late.
        loop {
            let names = mounted_names(&mount).await?;
            let freshness = engine.directory_freshness();
            if freshness.refreshing == 0 && freshness.delayed == 0 {
                // Not emptiness: the second phase runs in the same fixture root and
                // legitimately sees the first phase's folder. What must hold is that
                // this phase's own change is not there yet.
                anyhow::ensure!(
                    !names.iter().any(|e| e == OsString::from(child).as_os_str()),
                    "{child} already exists at baseline; retained for review"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        event(
            &mut *log,
            serde_json::json!({"stage":"baseline","phase":phase_name,"poll_seconds":poll_seconds}),
        )?;
        // A directory stays active for LEASE after it is used, and an active
        // directory is refetched every INTERVAL. The baseline listing above
        // therefore leaves this one revalidating for a minute, which is a third
        // way the change could be found and is what invalidated two earlier
        // versions of this fixture. Wait for that lease to lapse before touching
        // the cloud, so the mount is genuinely idle when the change lands.
        let idle_deadline = tokio::time::Instant::now() + Duration::from_secs(150);
        while engine.directory_freshness().active > 0 {
            anyhow::ensure!(
                tokio::time::Instant::now() < idle_deadline,
                "directory activity never lapsed; the phase cannot start idle"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        event(
            &mut *log,
            serde_json::json!({"stage":"idle","phase":phase_name,"listings_so_far":provider.listings.load(Ordering::SeqCst)}),
        )?;
        let created = graph.create_folder(scope, &root.id, child, &cancel).await?;
        let changed_at = tokio::time::Instant::now();
        event(
            &mut *log,
            serde_json::json!({"stage":"changed_behind_the_mount","phase":phase_name,"node":created}),
        )?;
        if !hidden_window.is_zero() {
            // Long enough that the provider's own delta carries the change, so
            // what is timed below is this mount's reaction rather than the
            // provider still catching up with itself. Deliberately without
            // reading the mount; see hold_without_watching.
            hold_without_watching(&provider, hidden_window).await?;
            event(
                &mut *log,
                serde_json::json!({"stage":"idle_hold_passed","phase":phase_name,"window_ms":hidden_window.as_millis()}),
            )?;
        }
        let deltas_before = provider.deltas.load(Ordering::SeqCst);
        let listings_before = provider.listings.load(Ordering::SeqCst);
        let enabled_at = tokio::time::Instant::now();
        if open_gate_after_change {
            gate.send(true).ok();
            event(
                &mut *log,
                serde_json::json!({"stage":"subscription_gate_opened","phase":phase_name}),
            )?;
        }
        // The mechanism under test ends in a delta request. Wait for that, not for
        // the mount, because listing the mount would itself refresh the directory
        // and take the discovery away from the mechanism being measured.
        let delta = await_delta(&provider, deltas_before, deadline).await?;
        let listings_before_delta = provider
            .listings
            .load(Ordering::SeqCst)
            .saturating_sub(listings_before);
        // Only now read the mount, to confirm the change actually reached it.
        let visible = await_entry(&mount, child, Duration::from_secs(30)).await?;
        let result = PhaseResult {
            delta_ms: delta.as_millis(),
            visible_ms: visible.as_millis(),
            connects: provider.connects.load(Ordering::SeqCst),
            listings_before_delta,
        };
        event(
            &mut *log,
            serde_json::json!({
                "stage":"discovered","phase":phase_name,
                "delta_after_enable_ms":result.delta_ms,
                "visible_after_delta_ms":result.visible_ms,
                "after_change_ms":changed_at.elapsed().as_millis(),
                "enable_to_change_ms":enabled_at.duration_since(changed_at).as_millis(),
                "subscription_connects":result.connects,
                "listings_before_delta":result.listings_before_delta,
            }),
        )?;
        Ok::<PhaseResult, anyhow::Error>(result)
    }
    .await;
    cancel.cancel();
    engine.stop().await;
    if let Some(session) = session {
        tokio::task::spawn_blocking(move || session.umount_and_join()).await??;
    }
    result
}
pub async fn onedrive_catchup(state: &Path, label: &str) -> Result<()> {
    let account = test_account(state, label)?;
    let _operation = accounts::account_operation(state, &account.id)?;
    let _owner = accounts::account_lock(&state.join("accounts").join(&account.id))?;
    let run = uuid::Uuid::new_v4();
    let directory = state.join("catchup-checks").join(run.to_string());
    crate::private_dir(&directory)?;
    let mut log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(directory.join("events.jsonl"))?;
    let scope = Scope {
        account: account.id.clone(),
        provider: "onedrive".into(),
        collection: account.drive.id.clone(),
    };
    let name = format!("Cirrove-Catchup-Validation-{run}");
    event(
        &mut log,
        serde_json::json!({"stage":"planned","folder":name,"scope":scope}),
    )?;
    for ancestor in directory.ancestors() {
        File::open(ancestor)?.sync_all()?;
    }
    println!(
        "Creating one new fixture folder; private evidence: {}",
        directory.display()
    );
    let graph = accounts::provider(&account)?;
    let cancel = CancellationToken::new();
    let _cancel_on_return = cancel.clone().drop_guard();
    let root = graph
        .create_folder(&scope, &account.root_id, &name, &cancel)
        .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"created_root","node":root}),
    )?;

    // Phase one. No subscription is connected while the change is made, and the
    // poll interval is longer than this process will live, so the only thing that
    // can surface the change is the catch-up a reconnection triggers.
    println!("Phase 1: change made with no subscription connected and polling out of reach.");
    let reconnect = phase(
        &directory,
        &account,
        graph.clone(),
        &scope,
        &root,
        &mut log,
        "reconnect",
        "made-while-disconnected",
        3600,
        true,
        Duration::from_secs(60),
        Duration::from_secs(120),
    )
    .await?;
    anyhow::ensure!(
        reconnect.connects == 1,
        "catch-up phase established {} subscriptions; exactly one reconnection must explain it",
        reconnect.connects
    );
    anyhow::ensure!(
        reconnect.listings_before_delta == 0,
        "{} directory listings ran before the catch-up delta; \
         an actively revalidated directory could explain the discovery instead",
        reconnect.listings_before_delta
    );
    println!(
        "Phase 1 passed: delta {} ms after the subscription connected, \
         entry listed {} ms later.",
        reconnect.delta_ms, reconnect.visible_ms
    );

    // Phase two. The subscription never connects at all, so nothing but the
    // ordinary periodic refresh can find this one.
    println!("Phase 2: change made with notifications never connected; only the poll can find it.");
    let periodic = phase(
        &directory,
        &account,
        graph.clone(),
        &scope,
        &root,
        &mut log,
        "periodic",
        "found-by-periodic-recovery",
        30,
        false,
        Duration::ZERO,
        Duration::from_secs(240),
    )
    .await?;
    anyhow::ensure!(
        periodic.connects == 0,
        "periodic phase connected {} subscriptions; a notification may explain its discovery",
        periodic.connects
    );
    anyhow::ensure!(
        periodic.listings_before_delta == 0,
        "{} directory listings ran before the periodic delta; \
         an actively revalidated directory could explain the discovery instead",
        periodic.listings_before_delta
    );
    println!(
        "Phase 2 passed: delta {} ms after the change, entry listed {} ms later, \
         by periodic refresh alone.",
        periodic.delta_ms, periodic.visible_ms
    );

    event(
        &mut log,
        serde_json::json!({
            "stage":"finished","passed":true,
            "reconnect_delta_ms":reconnect.delta_ms,"reconnect_visible_ms":reconnect.visible_ms,
            "periodic_delta_ms":periodic.delta_ms,"periodic_visible_ms":periodic.visible_ms,
            "poll_seconds":30,
            "fixture_retained":true,
        }),
    )?;
    println!(
        "Both mechanisms observed live. Fixture folder retained: {name}\n\
         Evidence: {}",
        directory.display()
    );
    Ok(())
}
