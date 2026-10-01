use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, Request as HttpRequest, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response as HttpResponse};
use axum::routing::get;
use dg_lab_link_core::{ControlCommand, ControlError, ControlService};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Semaphore, mpsc, watch};
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::LocalConfig;
use crate::wire::{HolderInfo, Operation, Request, Response, RuntimeInfo};
use crate::{HEARTBEAT_TIMEOUT, MAX_REQUEST_BYTES, REQUEST_TIMEOUT, SOCKET_WRITE_TIMEOUT};

struct Holder {
    info: HolderInfo,
    cancelled: CancellationToken,
}

struct Holders {
    entries: BTreeMap<String, Holder>,
    ever_held: bool,
}

pub(crate) struct Shared {
    pub service: ControlService,
    config: LocalConfig,
    directory: PathBuf,
    instance_id: String,
    holders: Mutex<Holders>,
    pub stopping: AtomicBool,
    shutdown_requested: CancellationToken,
    pub terminated: CancellationToken,
    pub normal_requests: Arc<Semaphore>,
    pub safety_requests: Arc<Semaphore>,
    websocket_clients: Arc<Semaphore>,
    stop_epoch: AtomicU64,
    epoch_updates: watch::Sender<u64>,
    enqueue_gate: std::sync::Mutex<()>,
}

#[derive(Clone, Copy)]
pub(crate) struct CommandEpoch(pub u64);

pub(crate) fn may_resume_output(command: &ControlCommand) -> bool {
    matches!(
        command,
        ControlCommand::StartOutput { .. }
            | ControlCommand::AdjustIntensity { .. }
            | ControlCommand::UpdateTouchInput { .. }
            | ControlCommand::AudioControl { .. }
            | ControlCommand::SetSyncAllDevices { .. }
            | ControlCommand::ConnectRelay
            | ControlCommand::RefreshPairing
            | ControlCommand::ConnectTransport { .. }
            | ControlCommand::RefreshConnectionPairing { .. }
            | ControlCommand::ConnectBluetooth { .. }
            | ControlCommand::SetBluetoothConfig { .. }
    )
}

impl Shared {
    fn new(service: ControlService, config: LocalConfig, directory: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            service,
            config,
            directory,
            instance_id: Uuid::new_v4().to_string(),
            holders: Mutex::new(Holders {
                entries: BTreeMap::new(),
                ever_held: false,
            }),
            stopping: AtomicBool::new(false),
            shutdown_requested: CancellationToken::new(),
            terminated: CancellationToken::new(),
            normal_requests: Arc::new(Semaphore::new(32)),
            safety_requests: Arc::new(Semaphore::new(16)),
            websocket_clients: Arc::new(Semaphore::new(32)),
            stop_epoch: AtomicU64::new(0),
            epoch_updates: watch::channel(0).0,
            enqueue_gate: std::sync::Mutex::new(()),
        })
    }

    pub fn accept_command(&self, safety: bool) -> CommandEpoch {
        let _gate = self
            .enqueue_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        CommandEpoch(if safety {
            let epoch = self.stop_epoch.fetch_add(1, Ordering::AcqRel) + 1;
            self.epoch_updates.send_replace(epoch);
            epoch
        } else {
            self.stop_epoch.load(Ordering::Acquire)
        })
    }

    #[cfg(test)]
    async fn execute(&self, command: ControlCommand) -> Result<Value, ControlError> {
        let epoch = self.accept_command(command.is_safety());
        self.execute_received(command, epoch).await
    }

    pub async fn execute_received(
        &self,
        command: ControlCommand,
        epoch: CommandEpoch,
    ) -> Result<Value, ControlError> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(ControlError::new(
                "runtime_stopping",
                "共享核心正在安全关闭",
            ));
        }
        let semaphore = if command.is_safety() {
            &self.safety_requests
        } else {
            &self.normal_requests
        };
        let _permit = semaphore.try_acquire().map_err(|_| busy())?;
        let gated = command.is_safety() || may_resume_output(&command);
        let resume = may_resume_output(&command);
        let mut first_poll = true;
        let mut execution = std::pin::pin!(self.service.execute(command));
        let guarded_execution = std::future::poll_fn(|context| {
            if !gated || !first_poll {
                return execution.as_mut().poll(context);
            }
            // Only enqueueing output/control takes this short synchronous gate.
            // No response wait, import parsing or persistence holds the gate.
            let _gate = self
                .enqueue_gate
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            first_poll = false;
            if resume && epoch.0 != self.stop_epoch.load(Ordering::Acquire) {
                return std::task::Poll::Ready(Err(ControlError::new(
                    "queue_busy",
                    "输出请求已被后续停止操作取消",
                )));
            }
            execution.as_mut().poll(context)
        });
        let value = timeout(REQUEST_TIMEOUT, guarded_execution)
            .await
            .map_err(|_| {
                ControlError::new(
                    "runtime_timeout",
                    "请求执行超时；写操作可能已执行，请读取状态后再决定是否重试",
                )
            })??;
        if serde_json::to_vec(&value)
            .map_err(|error| ControlError::new("runtime_protocol_error", error.to_string()))?
            .len()
            > crate::MAX_RESPONSE_BYTES - 4096
        {
            return Err(ControlError::new(
                "response_too_large",
                "共享核心结果超过 16 MiB；请按条目读取",
            ));
        }
        Ok(value)
    }

    async fn runtime_info(&self) -> RuntimeInfo {
        RuntimeInfo {
            instance_id: self.instance_id.clone(),
            pid: std::process::id(),
            holder_count: self.holders.lock().await.entries.len(),
            mcp_url: LocalConfig::saved_mcp_url(&self.directory)
                .unwrap_or_else(|_| self.config.mcp_url()),
        }
    }

    async fn register(
        &self,
        holder: HolderInfo,
        cancelled: CancellationToken,
    ) -> Result<(), ControlError> {
        validate_holder(&holder)?;
        let mut holders = self.holders.lock().await;
        if self.stopping.load(Ordering::Acquire) {
            return Err(ControlError::new(
                "runtime_stopping",
                "共享核心正在安全关闭",
            ));
        }
        if holders.entries.contains_key(&holder.id) {
            return Err(ControlError::new(
                "holder_already_exists",
                "持有者 ID 已存在",
            ));
        }
        holders.ever_held = true;
        holders.entries.insert(
            holder.id.clone(),
            Holder {
                info: holder,
                cancelled,
            },
        );
        Ok(())
    }

    async fn release_holder(&self, id: &str, cancel_socket: bool) -> Result<(), ControlError> {
        let mut holders = self.holders.lock().await;
        let holder = holders
            .entries
            .remove(id)
            .ok_or_else(|| ControlError::new("holder_not_found", "找不到指定持有者"))?;
        if cancel_socket {
            holder.cancelled.cancel();
        }
        if holders.entries.is_empty() && holders.ever_held {
            self.stopping.store(true, Ordering::Release);
            self.shutdown_requested.cancel();
        }
        Ok(())
    }
}

pub async fn run_core(
    directory: PathBuf,
    port: Option<u16>,
    relay_endpoint: Option<String>,
) -> Result<(), ControlError> {
    std::fs::create_dir_all(&directory)?;
    // Migrate a legacy shared HTTP/control port before acquiring our own core
    // lock; migration must not disturb an already running legacy process.
    let mut config = LocalConfig::load(&directory)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("core.lock"))?;
    lock.try_lock().map_err(|error| {
        ControlError::new(
            "core_already_running",
            format!("当前配置目录已有共享核心：{error}"),
        )
    })?;
    if let Some(port) = port {
        if port == 0 {
            return Err(ControlError::new("invalid_port", "本机端口必须大于零"));
        }
        config.port = port;
    }
    config.validate()?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.port))
        .await
        .map_err(|error| {
            ControlError::new(
                "runtime_bind_failed",
                format!(
                    "无法监听 127.0.0.1:{}，端口可能已被占用：{error}",
                    config.port
                ),
            )
        })?;
    // Persist only after successfully binding; a failed launch does not break clients.
    if port.is_some() {
        config = LocalConfig::save_core_port(&directory, config.port)?;
    }
    let endpoint = relay_endpoint
        .unwrap_or_else(|| dg_lab_link_core::dglab::client::DEFAULT_RELAY_ENDPOINT.to_owned());
    let (service, runtime) = ControlService::create(directory.clone(), endpoint)?;
    let state = Shared::new(service, config, directory);
    let mut hub_task = tokio::spawn(runtime.run());
    let app = Router::new()
        .route("/control", get(websocket_upgrade))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state.clone());
    let terminated = state.terminated.clone();
    let listener = LimitedListener::new(listener);
    let mut server_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(terminated.cancelled_owned())
            .await
    });
    let idle_state = state.clone();
    let idle_task = tokio::spawn(async move {
        tokio::time::sleep(HEARTBEAT_TIMEOUT).await;
        let holders = idle_state.holders.lock().await;
        if !holders.ever_held {
            idle_state.stopping.store(true, Ordering::Release);
            idle_state.shutdown_requested.cancel();
        }
    });
    let mut server_failed = None;
    let mut hub_finished = false;
    tokio::select! {
        _ = state.shutdown_requested.cancelled() => {},
        _ = tokio::signal::ctrl_c() => {},
        result = &mut server_task => { server_failed = Some(result); },
        _ = &mut hub_task => { hub_finished = true; },
    }
    state.stopping.store(true, Ordering::Release);
    let cleanup = timeout(Duration::from_secs(10), state.service.shutdown()).await;
    state.terminated.cancel();
    idle_task.abort();
    if !hub_finished
        && timeout(Duration::from_secs(1), &mut hub_task)
            .await
            .is_err()
    {
        hub_task.abort();
    }
    if server_failed.is_none()
        && timeout(Duration::from_secs(1), &mut server_task)
            .await
            .is_err()
    {
        server_task.abort();
    }
    drop(lock);
    if let Some(result) = server_failed {
        return Err(ControlError::new(
            "runtime_server_failed",
            format!("本机接口异常退出：{result:?}"),
        ));
    }
    cleanup.map_err(|_| {
        ControlError::new("shutdown_timeout", "安全关闭超过 10 秒，共享核心已退出")
    })??;
    Ok(())
}

async fn authenticate(
    State(state): State<Arc<Shared>>,
    request: HttpRequest<Body>,
    next: Next,
) -> HttpResponse {
    if !valid_headers(request.headers(), &state.config) {
        let authorization = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok());
        let status = if authorization != Some(format!("Bearer {}", state.config.token).as_str()) {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::FORBIDDEN
        };
        return status.into_response();
    }
    if state.stopping.load(Ordering::Acquire) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    next.run(request).await
}

fn validate_holder(holder: &HolderInfo) -> Result<(), ControlError> {
    if holder.id.len() > 128 || holder.id.is_empty() || holder.label.len() > 256 || holder.pid == 0
    {
        return Err(ControlError::new(
            "invalid_holder",
            "持有者 ID、标签或 PID 无效",
        ));
    }
    Ok(())
}

fn valid_headers(headers: &HeaderMap, config: &LocalConfig) -> bool {
    if headers.get_all("authorization").iter().count() != 1
        || headers.get_all("host").iter().count() != 1
        || headers.get_all("origin").iter().count() > 1
    {
        return false;
    }
    let authorization = format!("Bearer {}", config.token);
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some(authorization.as_str())
    {
        return false;
    }
    let authorities = [
        format!("127.0.0.1:{}", config.port),
        format!("localhost:{}", config.port),
    ];
    if !headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| {
            authorities
                .iter()
                .any(|value| value.eq_ignore_ascii_case(host))
        })
    {
        return false;
    }
    match headers.get("origin") {
        None => true,
        Some(origin) => origin.to_str().ok().is_some_and(|origin| {
            authorities
                .iter()
                .any(|authority| origin == format!("http://{authority}"))
        }),
    }
}

async fn websocket_upgrade(
    State(state): State<Arc<Shared>>,
    upgrade: WebSocketUpgrade,
) -> HttpResponse {
    let Ok(permit) = state.websocket_clients.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    upgrade
        .max_message_size(MAX_REQUEST_BYTES)
        .max_frame_size(MAX_REQUEST_BYTES)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            websocket(socket, state).await
        })
        .into_response()
}

type Outgoing = (Response, bool);

async fn websocket(mut socket: WebSocket, state: Arc<Shared>) {
    let hello = timeout(Duration::from_secs(5), socket.recv()).await;
    let Some(Ok(Message::Text(text))) = hello.ok().flatten() else {
        return;
    };
    let Ok(Request { id, operation, .. }) = serde_json::from_str::<Request>(&text) else {
        return;
    };
    let (holder, holding) = match operation {
        Operation::Hello(holder) => (holder, true),
        Operation::Observe(holder) => (holder, false),
        _ => return,
    };
    let cancelled = CancellationToken::new();
    let registration = if holding {
        state.register(holder.clone(), cancelled.clone()).await
    } else {
        validate_holder(&holder)
    };
    if let Err(error) = registration {
        let _ = timeout(
            SOCKET_WRITE_TIMEOUT,
            socket.send(Message::Text(
                serde_json::to_string(&Response::result(id, Err(error)))
                    .unwrap()
                    .into(),
            )),
        )
        .await;
        return;
    }
    let response = Response::Hello {
        holder: holder.clone(),
        runtime: state.runtime_info().await,
        snapshot: Box::new(state.service.snapshot()),
        command_epoch: state.stop_epoch.load(Ordering::Acquire),
    };
    if !matches!(
        timeout(
            SOCKET_WRITE_TIMEOUT,
            socket.send(Message::Text(
                serde_json::to_string(&response).unwrap().into()
            ))
        )
        .await,
        Ok(Ok(()))
    ) {
        if holding {
            let _ = state.release_holder(&holder.id, false).await;
        }
        return;
    }
    let (writer, mut reader) = socket.split();
    let (responses, response_rx) = mpsc::channel::<Outgoing>(16);
    let (safety_responses, safety_rx) = mpsc::channel::<Outgoing>(8);
    let write_cancelled = cancelled.clone();
    let write_state = state.clone();
    let write_task = tokio::spawn(async move {
        write_socket(writer, response_rx, safety_rx, write_state, write_cancelled).await
    });
    let per_client = Arc::new(Semaphore::new(8));
    let per_client_safety = Arc::new(Semaphore::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    let mut last_seen = Instant::now();
    loop {
        let message = tokio::select! {
            biased;
            _ = cancelled.cancelled() => break,
            _ = state.terminated.cancelled() => break,
            _ = tokio::time::sleep_until(last_seen + HEARTBEAT_TIMEOUT) => break,
            _ = tasks.join_next(), if !tasks.is_empty() => continue,
            message = reader.next() => match message { Some(Ok(message)) => message, _ => break },
        };
        if matches!(message, Message::Close(_)) {
            break;
        }
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(request) = serde_json::from_str::<Request>(&text) else {
            if responses
                .try_send((
                    Response::result(
                        0,
                        Err(ControlError::new("invalid_request", "控制请求格式无效")),
                    ),
                    false,
                ))
                .is_err()
            {
                break;
            }
            continue;
        };
        last_seen = Instant::now();
        let id = request.id;
        match request.operation {
            Operation::Heartbeat => {
                if safety_responses
                    .try_send((Response::result(id, Ok(Value::Null)), false))
                    .is_err()
                {
                    break;
                }
            }
            Operation::Release => {
                let result = if holding {
                    state
                        .release_holder(&holder.id, false)
                        .await
                        .map(|_| Value::Null)
                } else {
                    Ok(Value::Null)
                };
                if safety_responses
                    .try_send((Response::result(id, result), true))
                    .is_err()
                {
                    break;
                }
            }
            Operation::ReleaseHolder { id: target } => {
                let self_release = holding && target == holder.id;
                let result = state
                    .release_holder(&target, !self_release)
                    .await
                    .map(|_| Value::Null);
                if safety_responses
                    .try_send((Response::result(id, result), self_release))
                    .is_err()
                {
                    break;
                }
            }
            Operation::RuntimeInfo => {
                if responses
                    .try_send((
                        Response::result(id, Ok(json!(state.runtime_info().await))),
                        false,
                    ))
                    .is_err()
                {
                    break;
                }
            }
            Operation::Holders => {
                let holders: Vec<_> = state
                    .holders
                    .lock()
                    .await
                    .entries
                    .values()
                    .map(|holder| holder.info.clone())
                    .collect();
                if responses
                    .try_send((Response::result(id, Ok(json!(holders))), false))
                    .is_err()
                {
                    break;
                }
            }
            Operation::Hello(_) | Operation::Observe(_) => {
                if responses
                    .try_send((
                        Response::result(
                            id,
                            Err(ControlError::new("invalid_request", "持有者已注册")),
                        ),
                        false,
                    ))
                    .is_err()
                {
                    break;
                }
            }
            Operation::Call(command) => {
                let safety = command.is_safety();
                let epoch = state.accept_command(safety);
                let queue = if safety {
                    safety_responses.clone()
                } else {
                    responses.clone()
                };
                let epoch = if !safety && may_resume_output(&command) {
                    let Some(epoch) = request.command_epoch else {
                        if queue
                            .try_send((
                                Response::result(
                                    id,
                                    Err(ControlError::new(
                                        "invalid_request",
                                        "输出请求缺少核心命令代次",
                                    )),
                                ),
                                false,
                            ))
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    };
                    CommandEpoch(epoch)
                } else {
                    epoch
                };
                let semaphore = if safety {
                    per_client_safety.clone()
                } else {
                    per_client.clone()
                };
                let Ok(permit) = semaphore.try_acquire_owned() else {
                    if queue
                        .try_send((Response::result(id, Err(busy())), false))
                        .is_err()
                    {
                        break;
                    }
                    continue;
                };
                let shared = state.clone();
                let close = cancelled.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let result = shared.execute_received(command, epoch).await;
                    if queue
                        .try_send((Response::result(id, result), false))
                        .is_err()
                    {
                        close.cancel();
                    }
                });
            }
        }
    }
    cancelled.cancel();
    tasks.abort_all();
    if holding {
        let _ = state.release_holder(&holder.id, false).await;
    }
    let _ = timeout(Duration::from_secs(1), write_task).await;
}

async fn write_socket(
    mut writer: futures_util::stream::SplitSink<WebSocket, Message>,
    mut responses: mpsc::Receiver<Outgoing>,
    mut safety_responses: mpsc::Receiver<Outgoing>,
    state: Arc<Shared>,
    cancelled: CancellationToken,
) {
    let mut snapshots = state.service.subscribe();
    let mut epochs = state.epoch_updates.subscribe();
    epochs.mark_changed();
    loop {
        let (mut response, close_after) = tokio::select! {
            biased;
            outgoing = safety_responses.recv() => match outgoing { Some(outgoing) => outgoing, None => break },
            _ = cancelled.cancelled() => break,
            _ = state.terminated.cancelled() => break,
            result = epochs.changed() => {
                if result.is_err() { break; }
                (Response::CommandEpoch { epoch: *epochs.borrow_and_update() }, false)
            },
            outgoing = responses.recv() => match outgoing { Some(outgoing) => outgoing, None => break },
            result = snapshots.changed() => {
                if result.is_err() { break; }
                (Response::Snapshot { snapshot: Box::new(snapshots.borrow_and_update().clone()) }, false)
            },
        };
        if let Response::Result { command_epoch, .. } = &mut response {
            *command_epoch = state.stop_epoch.load(Ordering::Acquire);
        }
        let Ok(text) = serde_json::to_string(&response) else {
            break;
        };
        if text.len() > crate::MAX_RESPONSE_BYTES {
            break;
        }
        if !matches!(
            timeout(
                SOCKET_WRITE_TIMEOUT,
                writer.send(Message::Text(text.into()))
            )
            .await,
            Ok(Ok(()))
        ) {
            break;
        }
        if close_after {
            break;
        }
    }
    let _ = timeout(SOCKET_WRITE_TIMEOUT, writer.close()).await;
    cancelled.cancel();
}

fn busy() -> ControlError {
    ControlError::new("runtime_busy", "共享核心请求容量已满，请读取状态后再试")
}

/// Bound live TCP connections as well as requests and WebSocket holders.
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

struct LimitedStream {
    stream: tokio::net::TcpStream,
    _permit: tokio::sync::OwnedSemaphorePermit,
    idle: std::pin::Pin<Box<tokio::time::Sleep>>,
    initial_headers: Option<Vec<u8>>,
    idle_span: Duration,
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
                .expect("连接容量不会关闭");
            match self.listener.accept().await {
                Ok((stream, address)) => {
                    let _ = stream.set_nodelay(true);
                    return (
                        LimitedStream {
                            stream,
                            _permit: permit,
                            idle: Box::pin(tokio::time::sleep(Duration::from_secs(5))),
                            initial_headers: Some(Vec::new()),
                            idle_span: Duration::from_secs(30),
                        },
                        address,
                    );
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

impl tokio::io::AsyncRead for LimitedStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        match std::pin::Pin::new(&mut self.stream).poll_read(context, buffer) {
            std::task::Poll::Ready(result) => {
                if buffer.filled().len() > before {
                    if let Some(headers) = &mut self.initial_headers {
                        let remaining = 8192usize.saturating_sub(headers.len());
                        let received = &buffer.filled()[before..];
                        headers.extend_from_slice(&received[..received.len().min(remaining)]);
                        if let Some(end) =
                            headers.windows(4).position(|window| window == b"\r\n\r\n")
                        {
                            let websocket = String::from_utf8_lossy(&headers[..end])
                                .to_ascii_lowercase()
                                .contains("\nupgrade: websocket");
                            if websocket {
                                self.idle_span = HEARTBEAT_TIMEOUT;
                            }
                            self.initial_headers = None;
                            let idle_span = self.idle_span;
                            self.idle.as_mut().reset(Instant::now() + idle_span);
                        } else if headers.len() == 8192 {
                            return std::task::Poll::Ready(Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "本机 HTTP 请求头超过 8 KiB",
                            )));
                        }
                        // Incomplete first headers retain their absolute five-second
                        // deadline, so drip-fed bytes cannot monopolize a TCP slot.
                    } else {
                        let idle_span = self.idle_span;
                        self.idle.as_mut().reset(Instant::now() + idle_span);
                    }
                }
                std::task::Poll::Ready(result)
            }
            std::task::Poll::Pending => match self.idle.as_mut().poll(context) {
                std::task::Poll::Ready(()) => std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "本机连接超时",
                ))),
                std::task::Poll::Pending => std::task::Poll::Pending,
            },
        }
    }
}

impl tokio::io::AsyncWrite for LimitedStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.stream).poll_write(context, buffer)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(context)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_lab_link_core::model::Channel;
    use dg_lab_link_core::sources::audio::AudioAction;

    fn shared() -> (Arc<Shared>, dg_lab_link_core::hub::HubRuntime, PathBuf) {
        let directory = std::env::temp_dir().join(format!("dglab-runtime-test-{}", Uuid::new_v4()));
        let config = LocalConfig::load(&directory).unwrap();
        let (service, runtime) =
            ControlService::create(directory.clone(), "ws://127.0.0.1:1/v4".to_owned()).unwrap();
        (
            Shared::new(service, config, directory.clone()),
            runtime,
            directory,
        )
    }

    #[tokio::test]
    async fn accepted_output_commands_not_yet_polled_are_cancelled_by_stop() {
        let (state, runtime, directory) = shared();
        let hub = tokio::spawn(runtime.run());
        for command in [
            ControlCommand::StartOutput {
                device_id: "device".to_owned(),
            },
            ControlCommand::AdjustIntensity {
                device_id: "device".to_owned(),
                channel: Channel::A,
                delta: 1,
            },
            ControlCommand::AudioControl {
                action: AudioAction::Play,
            },
            ControlCommand::SetSyncAllDevices {
                device_id: "device".to_owned(),
                enabled: true,
            },
            ControlCommand::ConnectRelay,
            ControlCommand::RefreshPairing,
        ] {
            let epoch = state.accept_command(false);
            // A received WS request can remain unpolled in its spawned task.
            let delayed_command = state.execute_received(command, epoch);
            state.execute(ControlCommand::EmergencyStop).await.unwrap();
            assert_eq!(delayed_command.await.unwrap_err().code, "queue_busy");
        }
        assert_eq!(
            state.service.snapshot().connection.state,
            dg_lab_link_core::hub::ConnectionState::Disconnected
        );
        state.service.shutdown().await.unwrap();
        hub.await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn saturated_ordinary_requests_and_unread_snapshots_do_not_block_stop() {
        let (state, runtime, directory) = shared();
        let hub = tokio::spawn(runtime.run());
        let _slow_snapshot_subscriber = state.service.subscribe();
        let _ordinary = state
            .normal_requests
            .clone()
            .acquire_many_owned(32)
            .await
            .unwrap();
        assert_eq!(
            state
                .execute(ControlCommand::GetHubSnapshot)
                .await
                .unwrap_err()
                .code,
            "runtime_busy"
        );
        timeout(
            Duration::from_secs(1),
            state.execute(ControlCommand::EmergencyStop),
        )
        .await
        .unwrap()
        .unwrap();
        assert_ne!(
            state.service.snapshot().output.state,
            dg_lab_link_core::hub::OutputState::Running
        );
        state.service.shutdown().await.unwrap();
        hub.await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
