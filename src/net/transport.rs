//! Transport abstraction: "something that can deliver a text message to the
//! server and yield the server's messages".
//!
//! The WebSocket implementation ([`WsTransport`]) is what talks to Laravel.
//! [`crate::net::mock::MockTransport`] implements the same trait for tests and
//! the `--headless` offline mode, so everything above the transport (handshake,
//! heartbeat, snapshot application) is exercised without a server.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{self, Message};

/// How messages are framed on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// JSON over WebSocket **text** frames (Laravel-friendly, default).
    JsonText,
    /// Reserved: future binary protocol.
    JsonBinary,
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("connection closed by peer (code {code:?})")]
    ClosedByPeer { code: Option<u16> },
    #[error("transport task stopped unexpectedly")]
    Stopped,
    #[error("message too large ({0} bytes)")]
    TooLarge(usize),
    #[error("socket I/O error: {0}")]
    Io(String),
}

/// Max size of a single inbound frame. Snapshots for big LAN matches should
/// stay well below this; a larger frame means something is wrong and we would
/// rather reconnect than allocate blindly.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// A bidirectional message link with the game server.
///
/// Implementations must:
/// * yield inbound text frames via [`MessageTransport::next_message`]
/// * return `Ok(None)` when the peer closed cleanly, `Err` on failure
/// * respect `Framing`
#[async_trait]
pub trait MessageTransport: Send {
    /// Receives the next inbound JSON payload. `Ok(None)` = clean end of stream.
    async fn next_message(&mut self) -> Result<Option<String>, TransportError>;

    /// Sends one encoded protocol message. Fails once the link is gone.
    async fn send(&mut self, payload: String) -> Result<(), TransportError>;

    /// Requests a clean shutdown (close frame, then drop).
    async fn close(&mut self) -> Result<(), TransportError>;
}

/// Forwards to the boxed transport, so `Box<dyn MessageTransport>` is itself a
/// [`MessageTransport`] (used by the session driver's type-erased factory).
#[async_trait]
impl MessageTransport for Box<dyn MessageTransport> {
    async fn next_message(&mut self) -> Result<Option<String>, TransportError> {
        (**self).next_message().await
    }

    async fn send(&mut self, payload: String) -> Result<(), TransportError> {
        (**self).send(payload).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        (**self).close().await
    }
}

/// WebSocket transport built on `tokio-tungstenite`.
///
/// Connection flow is Laravel Reverb-compatible:
/// * The URL is the app endpoint **including the app key**, e.g.
///   `ws://host:8080/app/<REVERB_APP_KEY>` (`game:serve` routes game frames
///   behind `/app/{key}`). A URL without the key is rejected with HTTP 403.
/// * `wss://` works out of the box (rustls + webpki roots), so the same client
///   can talk to a TLS-terminated proxy outside the LAN.
pub struct WsTransport {
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    framing: Framing,
}

impl WsTransport {
    /// Connects to the plain WebSocket endpoint (no query token).
    pub async fn connect_url(url: String) -> Result<Self, TransportError> {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio_tungstenite::connect_async(url.as_str()),
        )
        .await
        .map_err(|_| TransportError::Io(format!("websocket upgrade to {url} timed out")))?;
        let (stream, response) = result?;
        tracing::debug!(%url, status = ?response.status(), "websocket connected");
        Ok(Self {
            stream,
            framing: Framing::JsonText,
        })
    }

    fn frame(&self, payload: String) -> Message {
        match self.framing {
            Framing::JsonText => Message::Text(payload.into()),
            Framing::JsonBinary => Message::Binary(payload.into_bytes().into()),
        }
    }
}

/// Token is appended for direct (non-Echo) connections. The `hello` message
/// additionally carries the token, which is what Laravel validates.
///
/// Currently unused (`hello.token` is the contract), kept because a future
/// Reverb channel-auth setup may want the query parameter instead.
#[allow(dead_code)]
fn build_url(base: &str, auth: Option<&str>) -> Result<String, TransportError> {
    let Some(auth) = auth.filter(|a| !a.is_empty()) else {
        return Ok(base.to_owned());
    };
    let separator = if base.contains('?') { '&' } else { '?' };
    Ok(format!("{base}{separator}auth={}", percent_encode(auth)))
}

/// Minimal query-parameter encoder – tokens are `[A-Za-z0-9._~|-]` in practice
/// (Sanctum), anything else is percent-encoded.
#[allow(dead_code)]
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[async_trait]
impl MessageTransport for WsTransport {
    async fn next_message(&mut self) -> Result<Option<String>, TransportError> {
        loop {
            match self.stream.next().await {
                None => return Ok(None),
                Some(Err(tungstenite::Error::ConnectionClosed)) => return Ok(None),
                Some(Err(tungstenite::Error::Io(io))) if is_disconnect(&io) => return Ok(None),
                Some(Err(err)) => return Err(TransportError::WebSocket(err)),
                Some(Ok(Message::Text(text))) => return Ok(Some(text.to_string())),
                Some(Ok(Message::Binary(bin))) => {
                    // Tolerate binary JSON – some proxies re-frame messages.
                    if bin.len() > MAX_MESSAGE_BYTES {
                        return Err(TransportError::TooLarge(bin.len()));
                    }
                    return Ok(Some(String::from_utf8_lossy(&bin).into_owned()));
                }
                // WebSocket control frames are handled by tungstenite itself.
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
                Some(Ok(Message::Close(frame))) => {
                    return Err(TransportError::ClosedByPeer {
                        code: frame.map(|f| u16::from(f.code)),
                    });
                }
                Some(Ok(Message::Frame(_))) => continue,
            }
        }
    }

    async fn send(&mut self, payload: String) -> Result<(), TransportError> {
        if payload.len() > MAX_MESSAGE_BYTES {
            return Err(TransportError::TooLarge(payload.len()));
        }
        let frame = self.frame(payload);
        self.stream.send(frame).await?;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        let _ = self.stream.close(None).await;
        Ok(())
    }
}

/// Errors that simply mean "the peer is gone" – not worth surfacing as errors.
fn is_disconnect(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::BrokenPipe
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(dead_code)]
    fn url_without_token_is_unchanged() {
        assert_eq!(
            build_url("ws://localhost/app", None).unwrap(),
            "ws://localhost/app"
        );
    }

    #[test]
    #[allow(dead_code)]
    fn token_is_appended_as_query() {
        assert_eq!(
            build_url("ws://localhost/app?x=1", Some("abc")).unwrap(),
            "ws://localhost/app?x=1&auth=abc"
        );
    }

    #[test]
    #[allow(dead_code)]
    fn token_is_percent_encoded() {
        assert_eq!(percent_encode("a b&c"), "a%20b%26c");
    }
}
