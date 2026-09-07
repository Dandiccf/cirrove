use super::*;
const MIB: u32 = 1024 * 1024;

async fn window(s: &GraphReadSession, size: u32) -> Result<u64, ProviderError> {
    let mut sink = WindowSink {
        bytes: 0,
        fail: false,
    };
    s.read_window(4 * MIB as u64, size, &mut sink, &CancellationToken::new())
        .await?;
    Ok(sink.bytes)
}

#[tokio::test]
async fn strong_window_streams_without_graph_checks_or_whole_window_allocation() {
    let f = fixture(128 * MIB as u64).await;
    let s = f.session();
    assert_eq!(s.window_limit(), 0);
    read(&s, 0, 32).await.unwrap();
    assert_eq!(s.window_limit(), 64 * MIB);
    assert_eq!(window(&s, 64 * MIB).await.unwrap(), 64 * MIB as u64);
    let c = f.graph.read_counters();
    assert_eq!(
        (
            c.graph_get_attempts,
            c.content_get_attempts,
            c.conditional_windows
        ),
        (2, 2, 1)
    );
    assert_eq!(c.content_body_bytes, 64 * MIB as u64 + 32);
}

#[tokio::test]
async fn ranges_and_windows_share_expired_or_rejected_binding_renewal() {
    for by_url in [false, true] {
        let f = fixture(128 * MIB as u64).await;
        let s = f.session();
        read(&s, 0, 32).await.unwrap();
        if by_url {
            f.remote.lock().unwrap().generation = 2;
        } else {
            expire(&s);
        }
        let mut tasks = JoinSet::new();
        for i in 0..8 {
            let s = s.clone();
            tasks.spawn(async move {
                if i % 2 == 0 {
                    assert_eq!(window(&s, 8 * MIB).await.unwrap(), 8 * MIB as u64);
                } else {
                    assert_eq!(read(&s, 64, 32).await.unwrap(), vec![b'A'; 32]);
                }
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        let c = f.graph.read_counters();
        assert_eq!((c.graph_get_attempts, c.setups, c.renewals), (4, 1, 1));
        // Renewal validates one requested transfer; no extra one-byte probes and
        // no second body appended to an already populated staging sink.
        assert_eq!(c.content_body_bytes, 4 * 8 * MIB as u64 + 5 * 32);
        if !by_url {
            assert_eq!(c.content_get_attempts, 9);
        }
    }
}

#[tokio::test]
async fn conditional_window_rejects_replacement_before_writing_any_staged_bytes() {
    for mode in ["normal", "ignore_condition"] {
        let f = fixture(16 * MIB as u64).await;
        let s = f.session();
        read(&s, 0, 32).await.unwrap();
        f.mode(mode);
        f.remote.lock().unwrap().version = 2;
        let mut sink = WindowSink {
            bytes: 0,
            fail: false,
        };
        assert!(matches!(
            s.read_window(
                4 * MIB as u64,
                8 * MIB,
                &mut sink,
                &CancellationToken::new()
            )
            .await,
            Err(ProviderError::VersionChanged)
        ));
        assert_eq!(sink.bytes, 0);
        assert_eq!(f.graph.read_counters().content_body_bytes, 32);
    }
}

#[tokio::test]
async fn failed_window_rebind_discards_the_single_stream_and_shares_failure() {
    let f = fixture(16 * MIB as u64).await;
    let s = f.session();
    read(&s, 0, 32).await.unwrap();
    expire(&s);
    f.remote.lock().unwrap().metadata = 0;
    f.mode("changed_during_setup");
    let mut sink = WindowSink {
        bytes: 0,
        fail: false,
    };
    assert!(matches!(
        s.read_window(
            4 * MIB as u64,
            8 * MIB,
            &mut sink,
            &CancellationToken::new()
        )
        .await,
        Err(ProviderError::VersionChanged)
    ));
    assert_eq!(sink.bytes, 8 * MIB as u64);
    let calls = f.graph.read_counters().content_get_attempts;
    assert!(matches!(
        window(&s, 8 * MIB).await,
        Err(ProviderError::VersionChanged)
    ));
    assert!(matches!(
        read(&s, 64, 32).await,
        Err(ProviderError::VersionChanged)
    ));
    assert_eq!(f.graph.read_counters().content_get_attempts, calls);
    assert_eq!(f.graph.read_counters().renewals, 0);
}

#[tokio::test]
async fn strong_window_response_or_sink_failure_never_restarts_after_partial_content() {
    for mode in [
        "wrong_range",
        "encoded",
        "ignore_range",
        "short",
        "oversize",
        "throttle",
        "normal",
    ] {
        let f = fixture(16 * MIB as u64).await;
        let s = f.session();
        read(&s, 0, 32).await.unwrap();
        f.mode(mode);
        let mut sink = WindowSink {
            bytes: 0,
            fail: mode == "normal",
        };
        let result = s
            .read_window(
                4 * MIB as u64,
                8 * MIB,
                &mut sink,
                &CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "{mode}");
        assert_eq!(f.graph.read_counters().content_get_attempts, 2, "{mode}");
        if ["wrong_range", "encoded", "ignore_range"].contains(&mode) {
            assert_eq!(sink.bytes, 0);
        }
        if mode == "throttle" {
            assert!(matches!(
                window(&s, 8 * MIB).await,
                Err(ProviderError::Throttled(_))
            ));
            assert!(matches!(
                read(&s, 64, 32).await,
                Err(ProviderError::Throttled(_))
            ));
            assert_eq!(f.graph.read_counters().content_get_attempts, 2);
        }
    }
}

struct InterruptingSink {
    bytes: u64,
    cancel: Option<CancellationToken>,
}
#[async_trait]
impl ReadWindowSink for InterruptingSink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        assert!(bytes.len() <= 64 * 1024);
        let first = self.bytes == 0;
        self.bytes += bytes.len() as u64;
        if first {
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
                std::future::pending::<()>().await;
            } else {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
        Ok(())
    }
}

#[tokio::test]
async fn lease_expiry_or_cancellation_after_streaming_starts_discards_without_append() {
    for cancelled in [false, true] {
        let f = fixture(16 * MIB as u64).await;
        let s = f.session();
        read(&s, 0, 32).await.unwrap();
        let cancel = CancellationToken::new();
        if !cancelled {
            let mut state = s.state.lock().unwrap();
            let State::Bound(bound) = &*state else {
                panic!("missing binding")
            };
            let mut bound = (**bound).clone();
            bound.expires = Instant::now() + Duration::from_secs(1);
            *state = State::Bound(Arc::new(bound));
        }
        let mut sink = InterruptingSink {
            bytes: 0,
            cancel: cancelled.then(|| cancel.clone()),
        };
        let result = s
            .read_window(4 * MIB as u64, 8 * MIB, &mut sink, &cancel)
            .await;
        if cancelled {
            assert!(matches!(result, Err(ProviderError::Cancelled)));
        } else {
            assert!(matches!(result, Err(ProviderError::Unavailable)));
        }
        assert!(sink.bytes > 0 && sink.bytes <= 8 * MIB as u64);
        assert_eq!(f.graph.read_counters().content_get_attempts, 2);
        assert_eq!(f.graph.read_counters().graph_get_attempts, 2);
        assert_eq!(window(&s, 8 * MIB).await.unwrap(), 8 * MIB as u64);
    }
}

#[tokio::test]
async fn renewed_origin_without_strong_validator_returns_to_checked_windows() {
    let f = fixture(16 * MIB as u64).await;
    let s = f.session();
    read(&s, 0, 32).await.unwrap();
    expire(&s);
    f.mode("weak");
    window(&s, 8 * MIB).await.unwrap();
    assert!(matches!(s.state().unwrap(), State::Fallback));
    assert_eq!(f.graph.read_counters().graph_get_attempts, 4);
    window(&s, 8 * MIB).await.unwrap();
    assert_eq!(f.graph.read_counters().graph_get_attempts, 6);
}
