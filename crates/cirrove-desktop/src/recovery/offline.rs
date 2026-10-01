//! Bounded local recovery for a disabled account. Call only on blocking workers.
use anyhow::{Result, ensure};
use cirrove_core::CancellationToken;
use cirrove_service::accounts::OfflineRecovery;
use std::path::Path;

pub use super::Selection;
#[derive(Clone, Default)]
pub struct Cursor {
    saved_after: u64,
    working_after: Option<String>,
    saved_done: bool,
    working_done: bool,
}
pub struct Page {
    pub selections: Vec<Selection>,
    pub next: Option<Cursor>,
}
fn open(state: &Path, label: &str, id: &str) -> Result<OfflineRecovery> {
    let recovery = OfflineRecovery::open(state, label)?;
    ensure!(recovery.account_id() == id, "the selected account changed");
    Ok(recovery)
}
pub fn load(state: &Path, label: &str, id: &str, mut cursor: Cursor) -> Result<Page> {
    let recovery = open(state, label, id)?;
    let mut selections = Vec::new();
    if !cursor.saved_done {
        let saved = recovery.list(cursor.saved_after, 200)?;
        cursor.saved_done = saved.len() < 200;
        if let Some(last) = saved.last() {
            cursor.saved_after = last.sequence;
        }
        selections.extend(
            saved
                .into_iter()
                .filter(super::eligible)
                .map(Selection::Saved),
        );
    }
    if !cursor.working_done {
        let (working, next) = recovery.working_list(
            cursor
                .working_after
                .as_deref()
                .map(str::parse)
                .transpose()?,
            200,
        )?;
        cursor.working_done = next.is_none();
        cursor.working_after = next.map(|id| id.to_string());
        selections.extend(working.into_iter().map(Selection::Working));
    }
    Ok(Page {
        selections,
        next: (!(cursor.saved_done && cursor.working_done)).then_some(cursor),
    })
}
fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
pub fn copy(
    state: &Path,
    label: &str,
    id: &str,
    selection: &Selection,
    destination: &Path,
    cancel: &CancellationToken,
    progress: impl FnMut(u64),
) -> Result<()> {
    let recovery = open(state, label, id)?;
    match selection {
        Selection::Saved(save) => {
            ensure!(super::eligible(save), "the selected save is unavailable");
            let receipt = recovery.export(
                save.operation
                    .ok_or_else(|| anyhow::anyhow!("missing save identity"))?,
                destination,
                cancel,
                progress,
            )?;
            ensure!(
                Some(receipt.operation) == save.operation
                    && receipt.size == save.size
                    && receipt.destination == destination
                    && digest(&receipt.sha256),
                "export receipt did not confirm the selected save"
            );
        }
        Selection::Working(working) => {
            let receipt = recovery.export_working(
                working.file,
                working.generation,
                destination,
                cancel,
                progress,
            )?;
            ensure!(
                receipt.source.file == working.file
                    && receipt.source.generation == working.generation
                    && receipt.source.size == working.size
                    && receipt.destination == destination
                    && digest(&receipt.sha256),
                "export receipt did not confirm the selected working file"
            );
        }
    }
    Ok(())
}
