#![allow(clippy::unwrap_used)]
use super::*;
use crate::folder_create::container_tests::{entry, fixture, node, scope};
#[tokio::test]
async fn file_move_refuses_app_owned_destination_before_mutation() {
    for kind in ["APP_CONTAINER", "APP_LIBRARY"] {
        let before = Node {
            id: "FILE::com.apple.CloudDocs::source".into(),
            kind: NodeKind::File,
            size: 3,
            ..node("source", ROOT_ID)
        };
        let destination = node("destination", ROOT_ID);
        let (session, server) =
            fixture(vec![(ROOT_ID.into(), vec![entry(&destination, kind)])]).await;
        let p = ICloudFileMove {
            scope: scope(),
            before: before.clone(),
            destination: destination.clone(),
            expected_sha256: "a".repeat(64),
            session: Mutex::new(SessionState::Ready(Box::new(session))),
            #[cfg(feature = "write-probe")]
            reconciliation_only: false,
        };
        let request = MutationRequest {
            scope: p.scope.clone(),
            intent: MutationIntent::Relocate {
                before: before.clone(),
                parent: destination.id,
                name: before.name.clone(),
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
