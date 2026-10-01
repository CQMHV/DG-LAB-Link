use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dg_lab_link_core::ControlError;
use futures_util::StreamExt;
use rmcp::model::{
    ClientJsonRpcMessage, ClientNotification, ClientRequest, GetExtensions, JsonRpcError,
    JsonRpcMessage, RequestId, ServerJsonRpcMessage,
};
use rmcp::transport::Transport;
use rmcp::transport::async_rw::{JsonRpcMessageCodec, JsonRpcMessageCodecError};
use rmcp::{ErrorData, RoleServer, ServiceExt};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout;
use tokio_util::codec::FramedRead;
use tokio_util::sync::CancellationToken;

use crate::handler::{CommandEpoch, McpServer, TransportKind};
use crate::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, SOCKET_WRITE_TIMEOUT};
use dg_lab_link_runtime::Client;

struct PendingRequest {
    _permit: Arc<OwnedSemaphorePermit>,
    safety: bool,
}

#[derive(Clone)]
struct StdioRequestPermit {
    _permit: Arc<OwnedSemaphorePermit>,
}

#[derive(Default)]
struct TransportState {
    closed: CancellationToken,
    fault: Mutex<Option<ControlError>>,
}

impl TransportState {
    fn fail(&self, error: ControlError) {
        self.fault
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_or_insert(error);
        self.closed.cancel();
    }
}

/// The SDK's JSON codec and service handle protocol negotiation and dispatch.
/// Bound both request handlers and concurrent response writes, with reserved
/// capacity for stops. Reading never waits for a slow stdout writer.
struct StdioTransport<R, W> {
    reader: FramedRead<R, JsonRpcMessageCodec<ClientJsonRpcMessage>>,
    writer: Arc<tokio::sync::Mutex<W>>,
    pending: Arc<Mutex<HashMap<RequestId, PendingRequest>>>,
    requests: Arc<Semaphore>,
    safety_requests: Arc<Semaphore>,
    responses: Arc<Semaphore>,
    safety_responses: Arc<Semaphore>,
    client: Client,
    state: Arc<TransportState>,
}

impl<R, W> StdioTransport<R, W> {
    fn new(reader: R, writer: W, client: Client, state: Arc<TransportState>) -> Self {
        Self {
            reader: FramedRead::new(
                reader,
                JsonRpcMessageCodec::new_with_max_length(MAX_REQUEST_BYTES),
            ),
            writer: Arc::new(tokio::sync::Mutex::new(writer)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            requests: Arc::new(Semaphore::new(32)),
            safety_requests: Arc::new(Semaphore::new(8)),
            responses: Arc::new(Semaphore::new(32)),
            safety_responses: Arc::new(Semaphore::new(8)),
            client,
            state,
        }
    }
}

impl<R, W> Transport<RoleServer> for StdioTransport<R, W>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    type Error = io::Error;

    fn send(
        &mut self,
        item: ServerJsonRpcMessage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let id = match &item {
            JsonRpcMessage::Response(response) => Some(&response.id),
            JsonRpcMessage::Error(error) => error.id.as_ref(),
            _ => None,
        };
        let request = id.and_then(|id| {
            self.pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(id)
        });
        let queue = if request.as_ref().is_some_and(|request| request.safety) {
            &self.safety_responses
        } else {
            &self.responses
        };
        // Acquire before returning the future: the SDK spawns response tasks.
        // Waiting inside those tasks would allow an unbounded response queue.
        let permit = queue.clone().try_acquire_owned();
        if permit.is_err() && !self.state.closed.is_cancelled() {
            self.state.fail(ControlError::new(
                "runtime_busy",
                "stdio 响应队列已满，正在释放本进程的持有者",
            ));
        }
        let writer = self.writer.clone();
        let state = self.state.clone();
        async move {
            let _request = request;
            let _permit = permit.map_err(|_| io::Error::other("stdio response queue is full"))?;
            if state.closed.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::NotConnected, "stdio closed"));
            }
            let mut bytes = serde_json::to_vec(&item).map_err(io::Error::other)?;
            if bytes.len() > MAX_RESPONSE_BYTES {
                state.fail(ControlError::new(
                    "response_too_large",
                    "stdio 响应超过 16 MiB",
                ));
                return Err(io::Error::other("stdio response exceeds 16 MiB"));
            }
            bytes.push(b'\n');
            match timeout(SOCKET_WRITE_TIMEOUT, async {
                let mut writer = writer.lock().await;
                writer.write_all(&bytes).await?;
                writer.flush().await
            })
            .await
            {
                Ok(Ok(())) => Ok(()),
                result => {
                    let error = match result {
                        Ok(Err(error)) => error,
                        Err(_) => {
                            io::Error::new(io::ErrorKind::TimedOut, "stdio stdout write timed out")
                        }
                        Ok(Ok(())) => unreachable!(),
                    };
                    state.fail(ControlError::new("stdio_write_failed", error.to_string()));
                    Err(error)
                }
            }
        }
    }

    async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
        loop {
            let incoming = tokio::select! {
                biased;
                _ = self.state.closed.cancelled() => return None,
                incoming = self.reader.next() => incoming,
            };
            let mut message = match incoming {
                Some(Ok(message)) => message,
                Some(Err(error)) => {
                    let code = if matches!(error, JsonRpcMessageCodecError::MaxLineLengthExceeded) {
                        "request_too_large"
                    } else {
                        "stdio_protocol_error"
                    };
                    self.state.fail(ControlError::new(code, error.to_string()));
                    return None;
                }
                None => {
                    self.state.closed.cancel();
                    return None;
                }
            };
            if let JsonRpcMessage::Request(request) = &mut message {
                let duplicate = self
                    .pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .contains_key(&request.id);
                if duplicate {
                    self.state
                        .fail(ControlError::new("invalid_request", "重复的 stdio 请求 ID"));
                    return None;
                }
                let safety = matches!(&request.request, ClientRequest::CallToolRequest(call)
                    if matches!(call.params.name.as_ref(), "emergency_stop" | "stop_output" | "disconnect_relay"));
                let queue = if safety {
                    &self.safety_requests
                } else {
                    &self.requests
                };
                let permit = match queue.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        let error = ControlError::new("runtime_busy", "stdio 请求队列已满");
                        let response = JsonRpcMessage::Error(JsonRpcError::new(
                            Some(request.id.clone()),
                            ErrorData::internal_error(error.message.clone(), Some(json!(error))),
                        ));
                        // The send future is bounded before spawning; receive
                        // remains free to accept a stop behind this request.
                        tokio::spawn(self.send(response));
                        continue;
                    }
                };
                let permit = Arc::new(permit);
                request.request.extensions_mut().insert(StdioRequestPermit {
                    _permit: permit.clone(),
                });
                self.pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(
                        request.id.clone(),
                        PendingRequest {
                            _permit: permit,
                            safety,
                        },
                    );
                let epoch = self.client.accept_command(safety);
                request.request.extensions_mut().insert(CommandEpoch(epoch));
            } else if let JsonRpcMessage::Notification(notification) = &message
                && let ClientNotification::CancelledNotification(cancelled) =
                    &notification.notification
                && let Some(id) = &cancelled.params.request_id
            {
                // The SDK omits responses to cancelled requests. Keep
                // the permit in the handler's extensions until it exits,
                // but remove the response reservation so it cannot leak.
                self.pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(id);
            }
            return Some(message);
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.state.closed.cancel();
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        Ok(())
    }
}

/// A stdio adapter owns one ordinary core holder for its entire connection.
/// EOF, interruption and transport errors all release that same holder.
pub async fn serve_stdio_mcp<R, W>(client: Client, reader: R, writer: W) -> Result<(), ControlError>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let state = Arc::new(TransportState::default());
    let transport = StdioTransport::new(reader, writer, client.clone(), state.clone());
    let result = async {
        let initialized = tokio::select! {
            biased;
            _ = client.closed() => return Err(core_closed()),
            _ = tokio::signal::ctrl_c() => return Ok(()),
            initialized = timeout(Duration::from_secs(30),
                McpServer::new(client.clone(), TransportKind::Stdio).serve_with_ct(transport, state.closed.clone())) => initialized,
        };
        let service = match initialized {
            Ok(Ok(service)) => service,
            Ok(Err(_)) if state.closed.is_cancelled() => return Ok(()),
            Ok(Err(error)) => return Err(ControlError::new("mcp_initialize_failed", error.to_string())),
            Err(_) => return Err(ControlError::new("mcp_initialize_timeout", "stdio MCP 初始化超过 30 秒")),
        };
        tokio::select! {
            biased;
            _ = client.closed() => Err(core_closed()),
            _ = tokio::signal::ctrl_c() => Ok(()),
            stopped = service.waiting() => stopped.map(|_| ())
                .map_err(|error| ControlError::new("mcp_service_failed", error.to_string())),
        }
    }.await;
    state.closed.cancel();
    let release = client.release().await;
    if let Some(error) = state
        .fault
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        return Err(error);
    }
    result.and(release)
}

fn core_closed() -> ControlError {
    ControlError::new("core_closed", "共享核心已关闭连接或释放本进程的持有者")
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_lab_link_core::ControlCommand;
    use tempfile::TempDir;
    use tokio::io::AsyncReadExt;

    struct CoreFixture {
        _directory: TempDir,
        client: Client,
        task: tokio::task::JoinHandle<Result<(), ControlError>>,
    }

    impl CoreFixture {
        async fn start() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let mcp_reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let mut config = dg_lab_link_runtime::LocalConfig::load(directory.path()).unwrap();
            config.port = reservation.local_addr().unwrap().port();
            config.mcp_port = mcp_reservation.local_addr().unwrap().port();
            config.save(directory.path()).unwrap();
            drop(reservation);
            drop(mcp_reservation);
            let task = tokio::spawn(dg_lab_link_runtime::run_core(
                directory.path().to_owned(),
                None,
                Some("ws://127.0.0.1:1/v4".to_owned()),
            ));
            let client = timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(client) =
                        Client::connect(directory.path(), "stdio transport test", None).await
                    {
                        break client;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            Self {
                _directory: directory,
                client,
                task,
            }
        }

        async fn finish(self) {
            self.client.release().await.unwrap();
            timeout(Duration::from_secs(12), self.task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }

    fn request(id: i64, name: &str) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": {}}
        }))
        .unwrap();
        bytes.push(b'\n');
        bytes
    }

    #[tokio::test]
    async fn oversized_stdio_frame_releases_the_last_holder() {
        let fixture = CoreFixture::start().await;
        let (host, mut peer) = tokio::io::duplex(64 * 1024);
        let (reader, writer) = tokio::io::split(host);
        let serve = tokio::spawn(serve_stdio_mcp(fixture.client.clone(), reader, writer));
        let oversized = vec![b' '; MAX_REQUEST_BYTES + 1];
        let _ = peer.write_all(&oversized).await;
        let error = timeout(Duration::from_secs(5), serve)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "request_too_large");
        fixture.finish().await;
    }

    #[tokio::test]
    async fn full_normal_queue_reserves_stop_capacity_and_cancels_old_output() {
        let fixture = CoreFixture::start().await;
        let (host, mut peer) = tokio::io::duplex(64 * 1024);
        let (reader, writer) = tokio::io::split(host);
        let state = Arc::new(TransportState::default());
        let mut transport =
            StdioTransport::new(reader, writer, fixture.client.clone(), state.clone());
        for id in 1..=32 {
            peer.write_all(&request(id, "get_hub_snapshot"))
                .await
                .unwrap();
        }
        let mut first = None;
        for _ in 0..32 {
            let message = transport.receive().await.unwrap();
            if first.is_none() {
                first = Some(message);
            }
        }
        let JsonRpcMessage::Request(first) = first.unwrap() else {
            panic!("expected request")
        };
        let old_epoch = first.request.extensions().get::<CommandEpoch>().unwrap().0;
        peer.write_all(&request(33, "get_hub_snapshot"))
            .await
            .unwrap();
        peer.write_all(&request(34, "emergency_stop"))
            .await
            .unwrap();
        let stop = timeout(Duration::from_secs(1), transport.receive())
            .await
            .unwrap()
            .unwrap();
        let JsonRpcMessage::Request(stop) = stop else {
            panic!("expected stop")
        };
        assert_eq!(stop.id, RequestId::Number(34));
        let epoch = stop.request.extensions().get::<CommandEpoch>().unwrap().0;
        assert_ne!(epoch, old_epoch);
        let stale = fixture
            .client
            .call_received(
                ControlCommand::StartOutput {
                    device_id: "missing-device".to_owned(),
                },
                old_epoch,
            )
            .await
            .unwrap_err();
        assert_eq!(stale.code, "queue_busy");
        fixture
            .client
            .call_received(ControlCommand::EmergencyStop, epoch)
            .await
            .unwrap();
        assert!(state.fault.lock().unwrap().is_none());
        // Cancellation does not free the handler's permit prematurely, and
        // dropping that handler releases it without waiting for an SDK response.
        peer.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":1}}\n").await.unwrap();
        transport.receive().await.unwrap();
        assert_eq!(transport.requests.available_permits(), 0);
        drop(first);
        assert_eq!(transport.requests.available_permits(), 1);
        transport.close().await.unwrap();
        drop(stop);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn blocked_stdout_does_not_delay_stop_and_has_a_deadline() {
        let fixture = CoreFixture::start().await;
        let (writer, mut slow_peer) = tokio::io::duplex(1);
        let (reader, mut input) = tokio::io::duplex(4096);
        let state = Arc::new(TransportState::default());
        let mut transport =
            StdioTransport::new(reader, writer, fixture.client.clone(), state.clone());
        let response = JsonRpcMessage::Error(JsonRpcError::new(
            Some(RequestId::Number(1)),
            ErrorData::internal_error("test", None),
        ));
        let sending = tokio::spawn(transport.send(response));
        // Once the first byte arrives, leave the rest unread so stdout blocks.
        timeout(Duration::from_secs(1), slow_peer.read_u8())
            .await
            .unwrap()
            .unwrap();
        input
            .write_all(&request(2, "emergency_stop"))
            .await
            .unwrap();
        timeout(Duration::from_millis(500), async {
            let JsonRpcMessage::Request(stop) = transport.receive().await.unwrap() else {
                panic!("expected stop")
            };
            let epoch = stop.request.extensions().get::<CommandEpoch>().unwrap().0;
            fixture
                .client
                .call_received(ControlCommand::EmergencyStop, epoch)
                .await
                .unwrap();
        })
        .await
        .expect("a slow stdout must not delay emergency stop");
        assert!(
            timeout(Duration::from_secs(2), sending)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert_eq!(
            state.fault.lock().unwrap().as_ref().unwrap().code,
            "stdio_write_failed"
        );
        assert!(state.closed.is_cancelled());
        assert_eq!(transport.responses.available_permits(), 32);
        transport.close().await.unwrap();
        fixture.finish().await;
    }
}
