//! Metadata and jobs from the active daemon; never open an active journal directly.
use super::Selection;
use anyhow::{Result, ensure};
use cirrove_service::{RecentReply, RecoveryWorkingReply};
use std::path::Path;

#[derive(Clone, Default)]
pub struct Cursor {
    pub working_after: Option<String>,
    pub saved_done: bool,
}
pub struct Page {
    pub selections: Vec<Selection>,
    pub next: Option<Cursor>,
    /// Older daemons can still offer saved generations without this new route.
    pub working_unavailable: bool,
}
pub async fn load(socket: &Path, label: &str, cursor: Cursor) -> Result<Page> {
    let saved = if cursor.saved_done {
        None
    } else {
        Some(
            cirrove_service::recent(
                socket,
                &cirrove_service::RecentRequest {
                    label: label.into(),
                    limit: 200,
                },
            )
            .await?,
        )
    };
    let working = cirrove_service::recovery_working(
        socket,
        &cirrove_service::RecoveryWorkingRequest {
            label: label.into(),
            after: cursor
                .working_after
                .as_deref()
                .map(str::parse)
                .transpose()?,
            limit: 200,
        },
    )
    .await
    .ok();
    page(cursor, saved, working)
}
/// Keep bounded pagination and partial older-daemon support testable without GTK.
pub fn page(
    cursor: Cursor,
    saved: Option<RecentReply>,
    working: Option<RecoveryWorkingReply>,
) -> Result<Page> {
    let mut selections = Vec::new();
    if let Some(saved) = saved {
        ensure!(saved.refusal.is_none(), "saved versions unavailable");
        ensure!(saved.local.len() <= 200, "saved page exceeds its limit");
        selections.extend(
            saved
                .local
                .into_iter()
                .filter(super::eligible)
                .map(Selection::Saved),
        );
    }
    let Some(working) = working.filter(|reply| reply.refusal.is_none()) else {
        return Ok(Page {
            selections,
            next: None,
            working_unavailable: true,
        });
    };
    ensure!(working.files.len() <= 200, "working page exceeds its limit");
    if let (Some(previous), Some(next)) = (cursor.working_after, working.next) {
        ensure!(
            next.to_string() > previous,
            "working cursor did not advance"
        );
    }
    selections.extend(working.files.into_iter().map(Selection::Working));
    Ok(Page {
        selections,
        next: working.next.map(|next| Cursor {
            working_after: Some(next.to_string()),
            saved_done: true,
        }),
        working_unavailable: false,
    })
}
pub async fn start(
    socket: &Path,
    label: &str,
    selection: &Selection,
    destination: &Path,
) -> Result<cirrove_service::ExportSaveReply> {
    match selection {
        Selection::Saved(save) => {
            ensure!(super::eligible(save), "selected save unavailable");
            cirrove_service::export_save(
                socket,
                &cirrove_service::ExportSaveRequest {
                    label: label.into(),
                    operation: save
                        .operation
                        .ok_or_else(|| anyhow::anyhow!("missing save identity"))?,
                    destination: destination.into(),
                },
            )
            .await
        }
        Selection::Working(working) => {
            cirrove_service::export_working(
                socket,
                &cirrove_service::ExportWorkingRequest {
                    label: label.into(),
                    file: working.file,
                    generation: working.generation,
                    destination: destination.into(),
                },
            )
            .await
        }
    }
}
