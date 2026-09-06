//! The Graph Socket.IO v5 / Engine.IO v4 WebSocket-only notification subset.
//! This is an independent implementation of the published protocols, not a sync
//! engine: payloads are ignored after identifying the notification event.
use super::*;
use cirrove_core::notifications::{ChangeHintSender, WatchEnd};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

const MAX_MESSAGE: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const RENEW_AFTER: Duration = Duration::from_secs(50 * 60);

#[derive(Deserialize)]
struct Subscription {
    #[serde(rename = "notificationUrl")]
    notification_url: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Handshake {
    sid: String,
    ping_interval: u64,
    ping_timeout: u64,
}

impl OneDrive {
    /// A current change marker for isolated notification validation. Unlike a
    /// baseline this intentionally omits existing items and must not seed a mount.
    pub async fn notification_checkpoint(
        &self,
        scope: &Scope,
        cancel: &CancellationToken,
    ) -> Result<Cursor, ProviderError> {
        let mut url = self.initial_url(&scope.collection)?;
        url.query_pairs_mut().append_pair("token", "latest");
        let page = self
            .changes(scope, Some(&Cursor(url.to_string())), cancel)
            .await?;
        match page.checkpoint {
            Checkpoint::Complete(cursor) if page.changes.is_empty() => Ok(cursor),
            _ => Err(ProviderError::Protocol(
                "notification check requires a current checkpoint",
            )),
        }
    }
    pub(super) async fn watch(
        &self,
        scope: &Scope,
        hints: ChangeHintSender,
        cancel: &CancellationToken,
    ) -> Result<WatchEnd, ProviderError> {
        if scope.account != self.account || scope.provider != "onedrive" {
            return Err(ProviderError::Protocol("provider/account mismatch"));
        }
        let url = self.resource_url(&[
            "drives",
            &scope.collection,
            "root",
            "subscriptions",
            "socketIo",
        ])?;
        // Only endpoint acquisition uses a background permit. The persistent
        // socket never occupies metadata/download capacity.
        let subscription: Subscription = {
            let _permit = self.budget.acquire(Priority::Background, cancel).await?;
            let body = tokio::select! {biased;
                _=cancel.cancelled()=>return Err(ProviderError::Cancelled),
                result=tokio::time::timeout(Duration::from_secs(45), self.request_bytes(url))=>
                    result.map_err(|_|ProviderError::Unavailable)??,
            };
            serde_json::from_slice(&body)
                .map_err(|_| ProviderError::Protocol("invalid notification endpoint"))?
        };
        let (url, namespace) = socket_url(
            &subscription.notification_url,
            self.endpoint.scheme() == "https",
        )?;
        let config = WebSocketConfig::default()
            .read_buffer_size(16 * 1024)
            .write_buffer_size(0)
            .max_write_buffer_size(128 * 1024)
            .max_frame_size(Some(MAX_MESSAGE))
            .max_message_size(Some(MAX_MESSAGE));
        // No bearer, cookies, redirect following or provider response logging.
        let (mut socket, _) = tokio::select! {biased;
            _=cancel.cancelled()=>return Err(ProviderError::Cancelled),
            result=tokio::time::timeout(CONNECT_TIMEOUT, connect_async_with_config(url.as_str(), Some(config), false))=>
                result.map_err(|_|ProviderError::Unavailable)?.map_err(|_|ProviderError::Unavailable)?,
        };
        session(&mut socket, &namespace, hints, cancel, RENEW_AFTER).await
    }
}

/// Match io(notificationUrl): URL path is the Socket.IO namespace; the Engine.IO
/// transport uses /socket.io/. The signed query remains only on that origin.
fn socket_url(raw: &str, secure: bool) -> Result<(Url, String), ProviderError> {
    if raw.len() > 16 * 1024 {
        return Err(ProviderError::Protocol("notification endpoint too long"));
    }
    let mut url =
        Url::parse(raw).map_err(|_| ProviderError::Protocol("invalid notification endpoint"))?;
    if !matches!(url.scheme(), "https" | "http")
        || (secure && url.scheme() != "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
    {
        return Err(ProviderError::Protocol("unsafe notification endpoint"));
    }
    let namespace = url.path().to_owned();
    if namespace.len() > 4096 || namespace.contains(',') {
        return Err(ProviderError::Protocol("invalid notification namespace"));
    }
    if url
        .query_pairs()
        .any(|(key, _)| matches!(key.as_ref(), "EIO" | "transport" | "sid"))
    {
        return Err(ProviderError::Protocol(
            "reserved notification transport parameter",
        ));
    }
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme)
        .map_err(|_| ProviderError::Protocol("invalid notification scheme"))?;
    url.set_path("/socket.io/");
    url.query_pairs_mut()
        .append_pair("EIO", "4")
        .append_pair("transport", "websocket");
    Ok((url, namespace))
}

struct Packet<'a> {
    kind: u8,
    namespace: &'a str,
    ack: &'a str,
    payload: &'a str,
}
fn packet(text: &str) -> Result<Packet<'_>, ProviderError> {
    let invalid = || ProviderError::Protocol("invalid notification packet");
    let Some((kind, mut rest)) = text.as_bytes().split_first() else {
        return Err(invalid());
    };
    if !matches!(kind, b'0'..=b'4') {
        return Err(invalid());
    }
    let mut namespace = "/";
    if rest.first() == Some(&b'/') {
        let comma = rest.iter().position(|b| *b == b',').ok_or_else(invalid)?;
        namespace = std::str::from_utf8(&rest[..comma]).map_err(|_| invalid())?;
        rest = &rest[comma + 1..];
    }
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits > 20 {
        return Err(invalid());
    }
    let ack = std::str::from_utf8(&rest[..digits]).map_err(|_| invalid())?;
    Ok(Packet {
        kind: *kind,
        namespace,
        ack,
        payload: std::str::from_utf8(&rest[digits..]).map_err(|_| invalid())?,
    })
}
fn prefix(namespace: &str) -> String {
    if namespace == "/" {
        String::new()
    } else {
        format!("{namespace},")
    }
}
async fn send<S>(
    socket: &mut WebSocketStream<S>,
    message: Message,
    cancel: &CancellationToken,
) -> Result<(), ProviderError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::select! {biased;
        _=cancel.cancelled()=>Err(ProviderError::Cancelled),
        result=tokio::time::timeout(Duration::from_secs(5), socket.send(message))=>
            result.map_err(|_|ProviderError::Unavailable)?.map_err(|_|ProviderError::Unavailable),
    }
}
async fn session<S>(
    socket: &mut WebSocketStream<S>,
    namespace: &str,
    hints: ChangeHintSender,
    cancel: &CancellationToken,
    renew_after: Duration,
) -> Result<WatchEnd, ProviderError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let connect_deadline = Instant::now() + CONNECT_TIMEOUT;
    let renew_deadline = Instant::now() + renew_after;
    let mut heartbeat_deadline = connect_deadline;
    let mut heartbeat = None;
    let mut connected = false;
    loop {
        let message = tokio::select! {biased;
            _=cancel.cancelled()=>return Err(ProviderError::Cancelled),
            _=tokio::time::sleep_until(renew_deadline)=>return Ok(WatchEnd::Renew),
            _=tokio::time::sleep_until(connect_deadline), if !connected=>return Err(ProviderError::Unavailable),
            _=tokio::time::sleep_until(heartbeat_deadline)=>return Err(ProviderError::Unavailable),
            message=socket.next()=>message.ok_or(ProviderError::Unavailable)?.map_err(|_|ProviderError::Unavailable)?,
        };
        let text = match message {
            Message::Text(text) => text,
            Message::Ping(bytes) => {
                send(socket, Message::Pong(bytes), cancel).await?;
                continue;
            }
            Message::Pong(_) => continue,
            _ => return Err(ProviderError::Unavailable),
        };
        if let Some(body) = text.strip_prefix('0') {
            if heartbeat.is_some() {
                return Err(ProviderError::Protocol("duplicate notification handshake"));
            }
            let open: Handshake = serde_json::from_str(body)
                .map_err(|_| ProviderError::Protocol("invalid notification handshake"))?;
            let ms = open
                .ping_interval
                .checked_add(open.ping_timeout)
                .filter(|ms| *ms > 0 && *ms <= 300_000)
                .ok_or(ProviderError::Protocol("invalid notification heartbeat"))?;
            if open.sid.is_empty()
                || open.sid.len() > 4096
                || open.ping_interval == 0
                || open.ping_timeout == 0
            {
                return Err(ProviderError::Protocol("invalid notification handshake"));
            }
            heartbeat = Some(Duration::from_millis(ms));
            heartbeat_deadline = Instant::now() + Duration::from_millis(ms);
            send(
                socket,
                Message::Text(format!("40{}", prefix(namespace)).into()),
                cancel,
            )
            .await?;
        } else if text == "2" {
            let interval = heartbeat.ok_or(ProviderError::Protocol(
                "notification heartbeat before handshake",
            ))?;
            heartbeat_deadline = Instant::now() + interval;
            send(socket, Message::Text("3".into()), cancel).await?;
        } else if let Some(body) = text.strip_prefix('4') {
            if heartbeat.is_none() {
                return Err(ProviderError::Protocol("notification before handshake"));
            }
            let p = packet(body)?;
            if p.namespace != namespace {
                return Err(ProviderError::Protocol("notification namespace mismatch"));
            }
            match p.kind {
                b'0' if !connected && p.ack.is_empty() => {
                    let data: serde_json::Value =
                        serde_json::from_str(p.payload).map_err(|_| {
                            ProviderError::Protocol("invalid subscription acknowledgement")
                        })?;
                    if !data
                        .get("sid")
                        .and_then(|s| s.as_str())
                        .is_some_and(|s| !s.is_empty())
                    {
                        return Err(ProviderError::Protocol(
                            "missing subscription acknowledgement",
                        ));
                    }
                    connected = true;
                    hints.connected();
                }
                b'2' if connected => {
                    let data: Vec<serde_json::Value> = serde_json::from_str(p.payload)
                        .map_err(|_| ProviderError::Protocol("invalid notification event"))?;
                    let name = data
                        .first()
                        .and_then(|v| v.as_str())
                        .ok_or(ProviderError::Protocol("invalid notification event name"))?;
                    if name == "notification" {
                        hints.changed();
                    }
                    if !p.ack.is_empty() {
                        send(
                            socket,
                            Message::Text(format!("43{}{}[]", prefix(namespace), p.ack).into()),
                            cancel,
                        )
                        .await?;
                    }
                }
                b'1' | b'4' => return Err(ProviderError::Unavailable),
                _ => return Err(ProviderError::Protocol("unexpected notification packet")),
            }
        } else {
            return Err(ProviderError::Unavailable);
        }
    }
}

#[cfg(test)]
mod tests;
