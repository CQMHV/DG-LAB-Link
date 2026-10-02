use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use dg_lab_link_core::ControlError;
use dg_lab_link_runtime::{Client, LocalConfig};
use futures_util::StreamExt;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, Sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::handler::{CommandEpoch, McpServer, TransportKind};
use crate::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, SOCKET_WRITE_TIMEOUT};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(16);

struct HttpState {
    client: Client,
    config: LocalConfig,
    closed: CancellationToken,
    ordinary: Arc<Semaphore>,
    safety: Arc<Semaphore>,
}

impl HttpState {
    fn new(client: Client, config: LocalConfig) -> Arc<Self> {
        Arc::new(Self {
            client,
            config,
            closed: CancellationToken::new(),
            ordinary: Arc::new(Semaphore::new(32)),
            safety: Arc::new(Semaphore::new(8)),
        })
    }
}

/// An HTTP adapter observes an existing core. Its connection and incoming HTTP
/// requests never add a holder; the caller owns the mcp-http.lock process lock.
pub async fn serve_http_mcp(client: Client, config: LocalConfig) -> Result<(), ControlError> {
    let listener = match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.mcp_port)).await {
        Ok(listener) => listener,
        Err(error) => {
            let _ = client.release().await;
            return Err(ControlError::new(
                "mcp_bind_failed",
                format!(
                    "无法监听 MCP 127.0.0.1:{}，端口可能已被占用：{error}",
                    config.mcp_port
                ),
            ));
        }
    };
    let state = HttpState::new(client, config);
    let app = router(state.clone());
    let closed = state.closed.clone();
    let mut server = tokio::spawn(async move {
        axum::serve(LimitedListener::new(listener), app)
            .with_graceful_shutdown(closed.cancelled_owned())
            .await
    });
    let mut server_finished = false;
    let result = tokio::select! {
        biased;
        _ = state.client.closed() => Err(ControlError::new("core_closed", "共享核心已关闭，HTTP MCP 接入正在退出")),
        _ = tokio::signal::ctrl_c() => Ok(()),
        outcome = &mut server => {
            server_finished = true;
            Err(ControlError::new("mcp_http_server_failed", format!("HTTP MCP 接口异常退出：{outcome:?}")))
        },
    };
    state.closed.cancel();
    // Close the observer explicitly: SDK handler clones may still be dropping.
    // Observers have no holder, so this cannot release another entry's session.
    let _ = state.client.release().await;
    if !server_finished && timeout(SOCKET_WRITE_TIMEOUT, &mut server).await.is_err() {
        server.abort();
    }
    result
}

fn router(state: Arc<HttpState>) -> Router {
    let mut transport_config = StreamableHttpServerConfig::default();
    transport_config.legacy_session_mode = false;
    transport_config.json_response = true;
    transport_config.max_request_body_bytes = MAX_REQUEST_BYTES;
    transport_config.cancellation_token = state.closed.clone();
    let service_client = state.client.clone();
    let service = StreamableHttpService::new(
        move || Ok(McpServer::new(service_client.clone(), TransportKind::Http)),
        Arc::new(LocalSessionManager::default()),
        transport_config,
    );
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(state, authenticate))
}

async fn authenticate(
    State(state): State<Arc<HttpState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !valid_headers(request.headers(), &state.config) {
        let expected = format!("Bearer {}", state.config.token);
        let authorized = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some(expected.as_str());
        return (if authorized {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::UNAUTHORIZED
        })
        .into_response();
    }
    if state.closed.is_cancelled() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let (mut parts, body) = request.into_parts();
    let bytes = match timeout(Duration::from_secs(3), to_bytes(body, MAX_REQUEST_BYTES)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
    };
    let safety = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .is_some_and(|message| {
            message["method"] == "tools/call"
                && message["params"]["name"].as_str().is_some_and(|name| {
                    crate::handler::priority_tool(
                        name,
                        message["params"]
                            .get("arguments")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({})),
                    )
                })
        });
    let queue = if safety {
        state.safety.clone()
    } else {
        state.ordinary.clone()
    };
    let permit = match queue.try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return StatusCode::TOO_MANY_REQUESTS.into_response(),
    };
    // Capture acceptance before SDK validation/dispatch. Both adapters use the
    // local/core epochs so stops from other entries also cancel delayed handlers.
    parts
        .extensions
        .insert(CommandEpoch(state.client.accept_command(safety)));
    let response = match timeout(
        REQUEST_TIMEOUT,
        next.run(Request::from_parts(parts, Body::from(bytes))),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => return StatusCode::GATEWAY_TIMEOUT.into_response(),
    };
    bounded_response(response, permit)
}

fn bounded_response(response: Response, permit: OwnedSemaphorePermit) -> Response {
    let (parts, body) = response.into_parts();
    let mut length = 0usize;
    let stream = body.into_data_stream().map(move |item| {
        // Hold capacity until the response stream is drained or disconnected,
        // rather than releasing it as soon as the SDK returns HTTP headers.
        let _reservation = &permit;
        let bytes = item.map_err(io::Error::other)?;
        length = length.saturating_add(bytes.len());
        if length > MAX_RESPONSE_BYTES {
            return Err(io::Error::other("MCP response exceeds 16 MiB"));
        }
        Ok(bytes)
    });
    Response::from_parts(parts, Body::from_stream(stream))
}

fn valid_headers(headers: &HeaderMap, config: &LocalConfig) -> bool {
    if headers.get_all("authorization").iter().count() != 1
        || headers.get_all("host").iter().count() != 1
        || headers.get_all("origin").iter().count() > 1
    {
        return false;
    }
    let expected = format!("Bearer {}", config.token);
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some(expected.as_str())
    {
        return false;
    }
    let hosts = [
        format!("127.0.0.1:{}", config.mcp_port),
        format!("localhost:{}", config.mcp_port),
    ];
    if !headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| {
            hosts
                .iter()
                .any(|expected| expected.eq_ignore_ascii_case(host))
        })
    {
        return false;
    }
    match headers.get("origin") {
        None => true,
        Some(origin) => origin
            .to_str()
            .ok()
            .is_some_and(|origin| hosts.iter().any(|host| origin == format!("http://{host}"))),
    }
}

/// Bound sockets, initial headers, idle reads and stalled writes independently
/// of SDK sessions. Incomplete headers retain an absolute five-second deadline.
struct LimitedListener {
    listener: TcpListener,
    connections: Arc<Semaphore>,
}

impl LimitedListener {
    fn new(listener: TcpListener) -> Self {
        Self {
            listener,
            connections: Arc::new(Semaphore::new(128)),
        }
    }
}

struct LimitedStream<S = TcpStream> {
    stream: S,
    _permit: OwnedSemaphorePermit,
    read_deadline: Pin<Box<Sleep>>,
    write_deadline: Option<Pin<Box<Sleep>>>,
    initial_headers: Option<Vec<u8>>,
}

impl axum::serve::Listener for LimitedListener {
    type Io = LimitedStream;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let permit = self
                .connections
                .clone()
                .acquire_owned()
                .await
                .expect("connection capacity remains open");
            match self.listener.accept().await {
                Ok((stream, address)) => {
                    let _ = stream.set_nodelay(true);
                    return (
                        LimitedStream {
                            stream,
                            _permit: permit,
                            read_deadline: Box::pin(tokio::time::sleep(Duration::from_secs(5))),
                            write_deadline: None,
                            initial_headers: Some(Vec::new()),
                        },
                        address,
                    );
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for LimitedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        match Pin::new(&mut self.stream).poll_read(context, buffer) {
            Poll::Ready(result) => {
                if buffer.filled().len() > before {
                    if let Some(headers) = &mut self.initial_headers {
                        let remaining = 8192usize.saturating_sub(headers.len());
                        let received = &buffer.filled()[before..];
                        headers.extend_from_slice(&received[..received.len().min(remaining)]);
                        if headers.windows(4).any(|window| window == b"\r\n\r\n") {
                            self.initial_headers = None;
                            self.read_deadline
                                .as_mut()
                                .reset(Instant::now() + Duration::from_secs(30));
                        } else if headers.len() == 8192 {
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "MCP HTTP headers exceed 8 KiB",
                            )));
                        }
                    } else {
                        self.read_deadline
                            .as_mut()
                            .reset(Instant::now() + Duration::from_secs(30));
                    }
                }
                Poll::Ready(result)
            }
            Poll::Pending => match self.read_deadline.as_mut().poll(context) {
                Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "MCP HTTP read timed out",
                ))),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

impl<S> LimitedStream<S> {
    fn write_wait(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let deadline = self
            .write_deadline
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(SOCKET_WRITE_TIMEOUT)));
        match deadline.as_mut().poll(context) {
            Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "MCP HTTP write timed out",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for LimitedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.stream).poll_write(context, buffer) {
            Poll::Ready(result) => {
                self.write_deadline = None;
                Poll::Ready(result)
            }
            Poll::Pending => match self.write_wait(context) {
                Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                _ => Poll::Pending,
            },
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.stream).poll_flush(context) {
            Poll::Ready(result) => {
                self.write_deadline = None;
                Poll::Ready(result)
            }
            Poll::Pending => self.write_wait(context),
        }
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_lab_link_core::ControlCommand;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::ServiceExt;

    #[tokio::test]
    async fn saturated_ordinary_http_capacity_still_disconnects_and_rejects_late_output() {
        let directory = tempfile::tempdir().unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mcp_reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = LocalConfig::load(directory.path()).unwrap();
        config.port = reservation.local_addr().unwrap().port();
        config.mcp_port = mcp_reservation.local_addr().unwrap().port();
        config.save(directory.path()).unwrap();
        drop((reservation, mcp_reservation));
        let core = tokio::spawn(dg_lab_link_runtime::run_core(
            directory.path().to_owned(),
            None,
            None,
        ));
        let gui = timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(client) =
                    Client::connect(directory.path(), "HTTP capacity test", None).await
                {
                    break client;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let observer = Client::connect_observer(directory.path()).await.unwrap();
        let state = HttpState::new(observer, config.clone());
        let app = router(state.clone());
        let accepted_before_stop = state.client.accept_command(false);
        let ordinary = state.ordinary.clone().acquire_many_owned(32).await.unwrap();
        let request = |name: &str| {
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Host", format!("127.0.0.1:{}", config.mcp_port))
                .header("Authorization", format!("Bearer {}", config.token))
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .header("MCP-Protocol-Version", "2025-06-18")
                .body(Body::from(
                    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                        "name": name, "arguments": {}
                    }})
                    .to_string(),
                ))
                .unwrap()
        };
        let ordinary_response = app
            .clone()
            .oneshot(request("get_hub_snapshot"))
            .await
            .unwrap();
        assert_eq!(ordinary_response.status(), StatusCode::TOO_MANY_REQUESTS);
        let stopped = timeout(
            Duration::from_secs(1),
            app.clone().oneshot(request("disconnect_relay")),
        )
        .await
        .expect("ordinary saturation cannot block a stop")
        .unwrap();
        assert_eq!(stopped.status(), StatusCode::OK);
        let stopped: Value = serde_json::from_slice(
            &to_bytes(stopped.into_body(), MAX_RESPONSE_BYTES)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(stopped["result"]["isError"], false, "{stopped}");
        // The HTTP request's Parts extension must reach the official SDK and
        // remote Client without accepting a fresh epoch inside the handler.
        assert_ne!(state.client.accept_command(false), accepted_before_stop);
        let stale = state
            .client
            .call_received(
                ControlCommand::StartOutput {
                    device_id: "missing-device".to_owned(),
                },
                accepted_before_stop,
            )
            .await
            .unwrap_err();
        assert_eq!(stale.code, "queue_busy");
        assert_eq!(state.safety.available_permits(), 8);
        assert_eq!(gui.runtime_info().await.unwrap().holder_count, 1);
        drop(ordinary);
        let available = app.oneshot(request("get_hub_snapshot")).await.unwrap();
        assert_eq!(available.status(), StatusCode::OK);
        to_bytes(available.into_body(), MAX_RESPONSE_BYTES)
            .await
            .unwrap();
        state.client.release().await.unwrap();
        gui.release().await.unwrap();
        timeout(Duration::from_secs(12), core)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    async fn streams() -> (LimitedStream, TcpStream) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let peer = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        (
            LimitedStream {
                stream,
                _permit: permit,
                read_deadline: Box::pin(tokio::time::sleep(Duration::from_millis(100))),
                write_deadline: None,
                initial_headers: Some(Vec::new()),
            },
            peer,
        )
    }

    #[tokio::test]
    async fn response_capacity_lasts_until_drain_and_oversize_is_bounded() {
        let capacity = Arc::new(Semaphore::new(1));
        let response = bounded_response(
            Response::new(Body::from("ready")),
            capacity.clone().acquire_owned().await.unwrap(),
        );
        assert_eq!(
            capacity.available_permits(),
            0,
            "headers do not release response capacity"
        );
        assert_eq!(
            to_bytes(response.into_body(), MAX_RESPONSE_BYTES)
                .await
                .unwrap(),
            "ready"
        );
        assert_eq!(capacity.available_permits(), 1);
        let response = bounded_response(
            Response::new(Body::from(vec![0u8; MAX_RESPONSE_BYTES + 1])),
            capacity.clone().acquire_owned().await.unwrap(),
        );
        assert!(
            to_bytes(response.into_body(), MAX_RESPONSE_BYTES + 1)
                .await
                .is_err()
        );
        assert_eq!(capacity.available_permits(), 1);
    }

    #[tokio::test]
    async fn incomplete_header_drips_do_not_extend_the_absolute_deadline() {
        let (mut stream, mut peer) = streams().await;
        let mut buffer = [0u8; 64];
        let header = b"GET /mcp HTTP/1.1\r\n";
        peer.write_all(header).await.unwrap();
        stream
            .read_exact(&mut buffer[..header.len()])
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        peer.write_all(b"H").await.unwrap();
        stream.read_exact(&mut buffer[..1]).await.unwrap();
        let error = timeout(Duration::from_millis(100), stream.read(&mut buffer))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn response_to_a_nonreading_stream_has_a_write_deadline() {
        // A bounded stream deterministically exercises the production
        // poll_write wrapper without OS TCP buffering hiding the stalled write.
        let (writer, mut peer) = tokio::io::duplex(1);
        let capacity = Arc::new(Semaphore::new(1));
        let mut stream = LimitedStream {
            stream: writer,
            _permit: capacity.clone().acquire_owned().await.unwrap(),
            read_deadline: Box::pin(tokio::time::sleep(Duration::from_secs(30))),
            write_deadline: None,
            initial_headers: Some(Vec::new()),
        };
        let start = Instant::now();
        let error = timeout(Duration::from_secs(3), stream.write_all(&[0u8; 1024]))
            .await
            .expect("an unread response cannot retain capacity indefinitely")
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() >= SOCKET_WRITE_TIMEOUT);
        assert_eq!(capacity.available_permits(), 0);
        drop(stream);
        assert_eq!(
            capacity.available_permits(),
            1,
            "closing the timed-out connection releases capacity"
        );
        assert_eq!(
            peer.read_u8().await.unwrap(),
            0,
            "the peer stayed connected and unread while the writer stalled"
        );
    }
}
