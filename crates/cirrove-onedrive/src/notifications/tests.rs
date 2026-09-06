#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::notifications::NotificationState;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_hdr_async;

#[test]
fn notification_urls_preserve_signed_query_without_confusing_namespace_and_transport() {
    let (url, namespace) =
        socket_url("https://notify.example.invalid/callback?secret=a%2Bb", true).unwrap();
    assert_eq!(namespace, "/callback");
    assert_eq!(url.scheme(), "wss");
    assert_eq!(url.path(), "/socket.io/");
    assert_eq!(
        url.query_pairs().collect::<Vec<_>>(),
        vec![
            ("secret".into(), "a+b".into()),
            ("EIO".into(), "4".into()),
            ("transport".into(), "websocket".into())
        ]
    );
    for raw in [
        "http://notify.invalid/callback",
        "https://user:pass@notify.invalid/callback",
        "https://notify.invalid/callback#token",
        "https://notify.invalid/callback?transport=polling",
        "https://notify.invalid/a,b",
    ] {
        assert!(socket_url(raw, true).is_err());
    }
    assert_eq!(
        packet("2/callback,17[\"notification\",{}]").unwrap().ack,
        "17"
    );
    assert!(packet("5/callback,1-[\"binary\"]").is_err());
}

#[allow(clippy::result_large_err)] // tungstenite fixes this callback's error type.
async fn sockets() -> (
    WebSocketStream<tokio::net::TcpStream>,
    WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        accept_hdr_async(
            stream,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert!(!request.headers().contains_key("authorization"));
                Ok(response)
            },
        )
        .await
        .unwrap()
    });
    let (client, _) = connect_async_with_config(
        format!("ws://{address}/socket.io/?EIO=4&transport=websocket"),
        Some(
            WebSocketConfig::default()
                .max_message_size(Some(MAX_MESSAGE))
                .max_frame_size(Some(MAX_MESSAGE)),
        ),
        false,
    )
    .await
    .unwrap();
    (server.await.unwrap(), client)
}
async fn open(server: &mut WebSocketStream<tokio::net::TcpStream>, interval: u64, timeout: u64) {
    server
        .send(Message::Text(
            format!(
                "0{{\"sid\":\"fixture\",\"pingInterval\":{interval},\"pingTimeout\":{timeout}}}"
            )
            .into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        server.next().await.unwrap().unwrap().into_text().unwrap(),
        "40/callback,"
    );
    server
        .send(Message::Text("40/callback,{\"sid\":\"subscribed\"}".into()))
        .await
        .unwrap();
}
#[tokio::test]
async fn websocket_subscription_heartbeats_notifications_acknowledgements_and_cancellation() {
    let (mut server, mut client) = sockets().await;
    let (hints, mut receiver) = ChangeHintSender::channel();
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let worker =
        tokio::spawn(
            async move { session(&mut client, "/callback", hints, &token, RENEW_AFTER).await },
        );
    open(&mut server, 1000, 1000).await;
    receiver.changed().await.unwrap();
    assert_eq!(
        receiver.borrow_and_update().state,
        NotificationState::Connected
    );
    let generation = receiver.borrow().generation;
    server.send(Message::Text("2".into())).await.unwrap();
    assert_eq!(
        server.next().await.unwrap().unwrap().into_text().unwrap(),
        "3"
    );
    server
        .send(Message::Text(
            "42/callback,17[\"notification\",{\"secret\":\"not-used\"}]".into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        server.next().await.unwrap().unwrap().into_text().unwrap(),
        "43/callback,17[]"
    );
    assert_eq!(receiver.borrow().generation, generation + 1);
    cancel.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap(),
        Err(ProviderError::Cancelled)
    ));
}
#[tokio::test]
async fn missing_engine_heartbeat_expires_even_while_other_frames_arrive() {
    let (mut server, mut client) = sockets().await;
    let (hints, _) = ChangeHintSender::channel();
    let worker = tokio::spawn(async move {
        session(
            &mut client,
            "/callback",
            hints,
            &CancellationToken::new(),
            RENEW_AFTER,
        )
        .await
    });
    open(&mut server, 50, 50).await;
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        server
            .send(Message::Text("42/callback,[\"notification\"]".into()))
            .await
            .unwrap();
    }
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(300), worker)
            .await
            .unwrap()
            .unwrap(),
        Err(ProviderError::Unavailable)
    ));
}
#[tokio::test]
async fn socket_bounds_messages_rejects_foreign_namespaces_and_renews() {
    for case in 0..3 {
        let (mut server, mut client) = sockets().await;
        let (hints, _) = ChangeHintSender::channel();
        let renewal = if case == 2 {
            Duration::from_millis(100)
        } else {
            RENEW_AFTER
        };
        let worker = tokio::spawn(async move {
            session(
                &mut client,
                "/callback",
                hints,
                &CancellationToken::new(),
                renewal,
            )
            .await
        });
        open(&mut server, 1000, 1000).await;
        match case {
            0 => server
                .send(Message::Text("42/foreign,[\"notification\"]".into()))
                .await
                .unwrap(),
            1 => server
                .send(Message::Text("x".repeat(MAX_MESSAGE + 1).into()))
                .await
                .unwrap(),
            _ => (),
        }
        let result = tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
        if case == 2 {
            assert!(matches!(result, Ok(WatchEnd::Renew)));
        } else {
            assert!(result.is_err());
        }
    }
}
