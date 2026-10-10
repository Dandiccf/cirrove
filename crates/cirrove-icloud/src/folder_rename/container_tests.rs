#![allow(clippy::unwrap_used)]
use super::*;
use crate::folder_create::container_tests::{entry, fixture, node, scope};
#[tokio::test]
async fn folder_rename_refuses_app_owned_source_before_mutation() {
    for kind in ["APP_CONTAINER", "APP_LIBRARY"] {
        let before = node("source", ROOT_ID);
        let (session, server) = fixture(vec![(ROOT_ID.into(), vec![entry(&before, kind)])]).await;
        let p = ICloudFolderRename {
            scope: scope(),
            before: before.clone(),
            target_name: "renamed".into(),
            session: Mutex::new(SessionState::Ready(Box::new(session))),
            #[cfg(feature = "write-probe")]
            reconciliation_only: false,
        };
        let request = MutationRequest {
            scope: p.scope.clone(),
            intent: MutationIntent::Relocate {
                before: before.clone(),
                parent: ROOT_ID.into(),
                name: "renamed".into(),
            },
        };
        assert!(matches!(
            p.mutate_prepared(&request, Some(&before.id), &CancellationToken::new())
                .await,
            Err(MutationError::Conflict)
        ));
        server.await.unwrap();
    }
}
#[tokio::test]
async fn folder_rename_reconciliation_does_not_accept_app_container_receipt() {
    for kind in ["APP_CONTAINER", "APP_LIBRARY", "FOLDER"] {
        let before = node("source", ROOT_ID);
        let mut after = before.clone();
        after.name = "renamed".into();
        let (session, server) = fixture(vec![(ROOT_ID.into(), vec![entry(&after, kind)])]).await;
        let p = ICloudFolderRename {
            scope: scope(),
            before,
            target_name: after.name,
            session: Mutex::new(SessionState::Ready(Box::new(session))),
            #[cfg(feature = "write-probe")]
            reconciliation_only: false,
        };
        let result = p.observe().await.unwrap();
        if kind == "FOLDER" {
            assert!(matches!(result, Observation::AtDestination(_)));
        } else {
            assert!(matches!(result, Observation::Conflict));
        }
        server.await.unwrap();
    }
}
