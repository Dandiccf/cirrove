#![allow(clippy::unwrap_used)]
use super::*;
use crate::folder_create::container_tests::{entry, fixture, node, scope};
#[tokio::test]
async fn folder_move_refuses_app_owned_source_or_destination_before_mutation() {
    for kind in ["APP_CONTAINER", "APP_LIBRARY"] {
        for changed in ["destination", "source", "source_again"] {
            let before = node("source", ROOT_ID);
            let destination = node("destination", ROOT_ID);
            let raw = entry(&before, kind);
            let plain = entry(&before, "FOLDER");
            let mut replies = vec![(
                ROOT_ID.into(),
                vec![entry(
                    &destination,
                    if changed == "destination" {
                        kind
                    } else {
                        "FOLDER"
                    },
                )],
            )];
            if changed != "destination" {
                replies.push((
                    ROOT_ID.into(),
                    vec![if changed == "source" {
                        raw.clone()
                    } else {
                        plain
                    }],
                ));
                replies.push((destination.id.clone(), vec![]));
                if changed == "source_again" {
                    replies.push((ROOT_ID.into(), vec![raw]));
                    replies.push((destination.id.clone(), vec![]));
                }
            }
            let (session, server) = fixture(replies).await;
            let p = ICloudFolderMove {
                scope: scope(),
                before: before.clone(),
                destination: destination.clone(),
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
            assert!(
                matches!(
                    p.mutate_prepared(&request, Some(&before.id), &CancellationToken::new())
                        .await,
                    Err(MutationError::Conflict)
                ),
                "{kind} {changed}"
            );
            server.await.unwrap();
        }
    }
}
#[tokio::test]
async fn ordinary_folder_move_still_prepares_after_matching_observations() {
    let before = node("source", ROOT_ID);
    let destination = node("destination", ROOT_ID);
    let (session, server) = fixture(vec![
        (ROOT_ID.into(), vec![entry(&destination, "FOLDER")]),
        (ROOT_ID.into(), vec![entry(&before, "FOLDER")]),
        (destination.id.clone(), vec![]),
        (ROOT_ID.into(), vec![entry(&before, "FOLDER")]),
        (destination.id.clone(), vec![]),
    ])
    .await;
    let p = ICloudFolderMove {
        scope: scope(),
        before: before.clone(),
        destination: destination.clone(),
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
    assert_eq!(
        p.prepare_mutation(&request, &CancellationToken::new())
            .await
            .unwrap(),
        Some(before.id)
    );
    server.await.unwrap();
}
