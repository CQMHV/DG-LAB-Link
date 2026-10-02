use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dg_lab_link_contracts::hub::{ChannelStatus, ConnectionState, HubSnapshot, OutputState};
use dg_lab_link_contracts::{ControlCommand, ControlError};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::LocalConfig;
use crate::wire::{HolderInfo, Operation, Request, Response, RuntimeInfo};
use crate::{HEARTBEAT_TIMEOUT, REQUEST_TIMEOUT, SOCKET_WRITE_TIMEOUT};

type Pending = Arc<std::sync::Mutex<BTreeMap<u64, oneshot::Sender<Result<Value, ControlError>>>>>;

struct PendingRequest {
    id: u64,
    pending: Pending,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.id);
    }
}

struct ClientRequest {
    request: Request,
    epoch: AcceptedCommandEpoch,
}

/// A transport acceptance token bound to the local queue and the last observed
/// core stop generation. Preserve it until forwarding the same command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceptedCommandEpoch {
    local: u64,
    core: u64,
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    holder_id: String,
    requests: mpsc::Sender<ClientRequest>,
    safety_requests: mpsc::Sender<ClientRequest>,
    pending: Pending,
    next_id: AtomicU64,
    snapshot: watch::Sender<HubSnapshot>,
    closed: CancellationToken,
    stop_epoch: Arc<AtomicU64>,
    core_epoch: Arc<AtomicU64>,
}

impl Drop for ClientInner {
    fn drop(&mut self) {
        self.closed.cancel();
    }
}

impl Client {
    /// Connect to an existing core and register one holder for this client.
    /// Clones share that holder; windows must clone the GUI process's client.
    pub async fn connect(
        directory: &Path,
        label: &str,
        holder_id: Option<&str>,
    ) -> Result<Self, ControlError> {
        Self::connect_mode(directory, label, holder_id, true).await
    }

    /// Connect without holding the core alive. HTTP MCP uses one observer
    /// connection and terminates when the existing GUI/CLI holders leave.
    pub async fn connect_observer(directory: &Path) -> Result<Self, ControlError> {
        Self::connect_mode(directory, "MCP HTTP", None, false).await
    }

    async fn connect_mode(
        directory: &Path,
        label: &str,
        holder_id: Option<&str>,
        holding: bool,
    ) -> Result<Self, ControlError> {
        let config = LocalConfig::load(directory)?;
        let mut request = format!("ws://127.0.0.1:{}/control", config.port)
            .into_client_request()
            .map_err(|error| ControlError::new("runtime_config_invalid", error.to_string()))?;
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", config.token).parse().map_err(
                |error: tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue| {
                    ControlError::new("runtime_config_invalid", error.to_string())
                },
            )?,
        );
        let socket_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(crate::MAX_RESPONSE_BYTES))
            .max_frame_size(Some(crate::MAX_RESPONSE_BYTES));
        let (mut socket, _) = timeout(
            Duration::from_secs(3),
            tokio_tungstenite::connect_async_with_config(request, Some(socket_config), true),
        )
        .await
        .map_err(|_| unavailable())?
        .map_err(|error| match &error {
            tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 401 => {
                ControlError::new("runtime_auth_failed", "共享核心令牌与当前配置不一致")
            }
            tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 503 => {
                ControlError::new("runtime_stopping", "共享核心正在安全关闭")
            }
            tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 429 => {
                ControlError::new("runtime_busy", "本机接口的连接容量已满")
            }
            tokio_tungstenite::tungstenite::Error::Http(response) => ControlError::new(
                "runtime_port_conflict",
                format!("本机端口上的接口拒绝连接：{}", response.status()),
            ),
            _ => unavailable(),
        })?;
        let holder = HolderInfo {
            id: holder_id
                .map(str::to_owned)
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
            label: label.to_owned(),
            pid: std::process::id(),
        };
        let hello = Request {
            id: 1,
            operation: if holding {
                Operation::Hello(holder)
            } else {
                Operation::Observe(holder)
            },
            command_epoch: None,
        };
        socket
            .send(Message::Text(
                serde_json::to_string(&hello)
                    .map_err(invalid_response)?
                    .into(),
            ))
            .await
            .map_err(|_| unavailable())?;
        let first = timeout(Duration::from_secs(5), socket.next())
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(unavailable)?
            .map_err(|_| unavailable())?;
        let (holder, snapshot, core_epoch) =
            match serde_json::from_slice::<Response>(&first.into_data())
                .map_err(invalid_response)?
            {
                Response::Hello {
                    holder,
                    snapshot,
                    command_epoch,
                    ..
                } => (holder, *snapshot, command_epoch),
                Response::Result {
                    error: Some(error), ..
                } => return Err(error),
                _ => {
                    return Err(ControlError::new(
                        "runtime_protocol_error",
                        "共享核心未返回持有者握手",
                    ));
                }
            };
        let (requests, mut requests_rx) = mpsc::channel::<ClientRequest>(32);
        let (safety_requests, mut safety_rx) = mpsc::channel::<ClientRequest>(8);
        let (snapshot, _) = watch::channel(snapshot);
        let pending: Pending = Arc::new(std::sync::Mutex::new(BTreeMap::new()));
        let closed = CancellationToken::new();
        let stop_epoch = Arc::new(AtomicU64::new(0));
        let core_epoch = Arc::new(AtomicU64::new(core_epoch));
        let inner = Arc::new(ClientInner {
            holder_id: holder.id,
            requests,
            safety_requests,
            pending: pending.clone(),
            next_id: AtomicU64::new(2),
            snapshot: snapshot.clone(),
            closed: closed.clone(),
            stop_epoch: stop_epoch.clone(),
            core_epoch: core_epoch.clone(),
        });
        let (mut writer, mut reader) = socket.split();
        let write_closed = closed.clone();
        let write_pending = pending.clone();
        tokio::spawn(async move {
            let mut heartbeat = tokio::time::interval(Duration::from_secs(2));
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let request = tokio::select! {
                    biased;
                    _ = write_closed.cancelled() => break,
                    request = safety_rx.recv() => match request { Some(request) => request, None => break },
                    _ = heartbeat.tick() => ClientRequest {
                        request: Request { id: 0, operation: Operation::Heartbeat, command_epoch: None },
                        epoch: AcceptedCommandEpoch { local: stop_epoch.load(Ordering::Acquire), core: 0 },
                    },
                    request = requests_rx.recv() => match request { Some(request) => request, None => break },
                };
                if matches!(&request.request.operation, Operation::Call(command) if command.may_resume_output())
                    && request.epoch.local != stop_epoch.load(Ordering::Acquire)
                {
                    if let Some(sender) = write_pending
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .remove(&request.request.id)
                    {
                        let _ = sender.send(Err(ControlError::new(
                            "queue_busy",
                            "输出请求已被后续停止操作取消",
                        )));
                    }
                    continue;
                }
                let Ok(text) = serde_json::to_string(&request.request) else {
                    break;
                };
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
            }
            let _ = timeout(SOCKET_WRITE_TIMEOUT, writer.close()).await;
            write_closed.cancel();
        });
        tokio::spawn(async move {
            loop {
                let message = tokio::select! {
                    _ = closed.cancelled() => break,
                    message = timeout(HEARTBEAT_TIMEOUT, reader.next()) => match message {
                        Ok(Some(Ok(message))) => message,
                        _ => break,
                    },
                };
                if matches!(message, Message::Close(_)) {
                    break;
                }
                if !matches!(message, Message::Text(_) | Message::Binary(_)) {
                    continue;
                }
                let Ok(response) = serde_json::from_slice::<Response>(&message.into_data()) else {
                    break;
                };
                match response {
                    Response::CommandEpoch { epoch } => {
                        core_epoch.fetch_max(epoch, Ordering::AcqRel);
                    }
                    Response::Snapshot {
                        snapshot: mut update,
                        configs,
                    } => {
                        restore_source_configs(&mut update, &snapshot.borrow(), configs);
                        snapshot.send_replace(*update);
                    }
                    Response::Result {
                        id,
                        result,
                        error,
                        command_epoch,
                    } => {
                        core_epoch.fetch_max(command_epoch, Ordering::AcqRel);
                        if let Some(sender) = pending
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(&id)
                        {
                            let _ = sender.send(match error {
                                Some(error) => Err(error),
                                None => Ok(result.unwrap_or(Value::Null)),
                            });
                        }
                    }
                    _ => {}
                }
            }
            closed.cancel();
            // Do not leave the GUI showing active output after a crashed core.
            let mut disconnected = snapshot.borrow().clone();
            disconnected.revision = disconnected.revision.saturating_add(1);
            for connection in &mut disconnected.connections {
                connection.state = ConnectionState::Error;
                connection.last_error = Some("共享核心连接已关闭".to_owned());
            }
            for binding in &mut disconnected.source_bindings {
                binding.binding.active = false;
            }
            for source in &mut disconnected.sources {
                if source.plugin_id.is_some() {
                    source.runtime_status = "stopped".into();
                }
            }
            disconnected.output.state = OutputState::Error;
            disconnected.output.last_error = Some("共享核心连接已关闭".to_owned());
            disconnected.output_device_count = 0;
            for device in &mut disconnected.devices {
                device.output_active = false;
                device.channel_a_status = ChannelStatus::Disconnected;
                device.channel_b_status = ChannelStatus::Disconnected;
            }
            snapshot.send_replace(disconnected);
            for (_, sender) in
                std::mem::take(&mut *pending.lock().unwrap_or_else(|error| error.into_inner()))
            {
                let _ = sender.send(Err(unavailable()));
            }
        });
        Ok(Self { inner })
    }

    pub async fn call(&self, command: ControlCommand) -> Result<Value, ControlError> {
        let safety = command.is_safety();
        self.request(Operation::Call(command), safety).await
    }

    /// Capture local and core stop epochs before a transport schedules its
    /// handler. Stops from any connected entry point invalidate older output.
    pub fn accept_command(&self, safety: bool) -> AcceptedCommandEpoch {
        let local = if safety {
            self.inner.stop_epoch.fetch_add(1, Ordering::AcqRel) + 1
        } else {
            self.inner.stop_epoch.load(Ordering::Acquire)
        };
        AcceptedCommandEpoch {
            local,
            core: self.inner.core_epoch.load(Ordering::Acquire),
        }
    }

    /// Forward a previously accepted command without changing its captured
    /// epoch. Callers must obtain the epoch from this same client's accept_command.
    pub async fn call_received(
        &self,
        command: ControlCommand,
        epoch: AcceptedCommandEpoch,
    ) -> Result<Value, ControlError> {
        let safety = command.is_safety();
        self.request_with_epoch(Operation::Call(command), safety, Some(epoch))
            .await
    }

    pub fn snapshot(&self) -> HubSnapshot {
        self.inner.snapshot.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<HubSnapshot> {
        self.inner.snapshot.subscribe()
    }
    pub fn holder_id(&self) -> &str {
        &self.inner.holder_id
    }
    pub async fn closed(&self) {
        self.inner.closed.cancelled().await
    }

    pub async fn release(&self) -> Result<(), ControlError> {
        if self.inner.closed.is_cancelled() {
            return Ok(());
        }
        let result = self.request(Operation::Release, true).await.map(|_| ());
        self.inner.closed.cancel();
        result
    }

    pub async fn runtime_info(&self) -> Result<RuntimeInfo, ControlError> {
        serde_json::from_value(self.request(Operation::RuntimeInfo, false).await?)
            .map_err(invalid_response)
    }

    pub async fn holders(&self) -> Result<Vec<HolderInfo>, ControlError> {
        serde_json::from_value(self.request(Operation::Holders, false).await?)
            .map_err(invalid_response)
    }

    pub async fn release_holder(&self, id: &str) -> Result<(), ControlError> {
        if id == self.holder_id() {
            return self.release().await;
        }
        self.request(Operation::ReleaseHolder { id: id.to_owned() }, true)
            .await
            .map(|_| ())
    }

    async fn request(&self, operation: Operation, safety: bool) -> Result<Value, ControlError> {
        self.request_with_epoch(operation, safety, None).await
    }

    async fn request_with_epoch(
        &self,
        operation: Operation,
        safety: bool,
        received_epoch: Option<AcceptedCommandEpoch>,
    ) -> Result<Value, ControlError> {
        if self.inner.closed.is_cancelled() {
            return Err(unavailable());
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let epoch = received_epoch.unwrap_or_else(|| {
            self.accept_command(
                matches!(&operation, Operation::Call(command) if command.is_safety())
                    || matches!(&operation, Operation::Release),
            )
        });
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self
                .inner
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if pending.len() >= if safety { 48 } else { 32 } {
                return Err(ControlError::new("runtime_busy", "本机请求队列已满"));
            }
            pending.insert(id, sender);
        }
        let _pending_request = PendingRequest {
            id,
            pending: self.inner.pending.clone(),
        };
        let queue = if safety {
            &self.inner.safety_requests
        } else {
            &self.inner.requests
        };
        let request = Request {
            id,
            operation,
            command_epoch: Some(epoch.core),
        };
        let size = serde_json::to_vec(&request)
            .map_err(invalid_response)?
            .len();
        if size > crate::MAX_REQUEST_BYTES
            || queue.try_send(ClientRequest { request, epoch }).is_err()
        {
            return Err(ControlError::new(
                if size > crate::MAX_REQUEST_BYTES {
                    "request_too_large"
                } else {
                    "runtime_busy"
                },
                "请求过大或本机请求队列已满",
            ));
        }
        let response = timeout(REQUEST_TIMEOUT, receiver).await;
        match response {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(unavailable()),
            Err(_) => Err(ControlError::new(
                "runtime_timeout",
                "共享核心请求超时；写操作可能已执行，请读取状态后再决定是否重试",
            )),
        }
    }
}

/// The core executable is distributed beside its GUI and CLI clients.
pub fn core_executable() -> Result<PathBuf, ControlError> {
    Ok(std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "dg-lab-link-core.exe"
    } else {
        "dg-lab-link-core"
    }))
}

/// Start the detached core if absent. Only connection establishment is retried;
/// business commands are never automatically replayed.
pub async fn connect_or_spawn(
    directory: &Path,
    executable: &Path,
    label: &str,
    holder_id: Option<&str>,
) -> Result<Client, ControlError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut child: Option<tokio::process::Child> = None;
    let mut stderr = None;
    loop {
        match Client::connect(directory, label, holder_id).await {
            Ok(client) => return Ok(client),
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "runtime_unavailable" | "runtime_port_conflict" | "runtime_stopping"
                ) => {}
            Err(error) => return Err(error),
        }
        if child.is_none() && core_lock_available(directory)? {
            let mut command = tokio::process::Command::new(executable);
            command
                .arg("--json")
                .arg("--config-dir")
                .arg(directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            #[cfg(windows)]
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.as_std_mut().process_group(0);
            }
            let mut spawned = command.spawn().map_err(|error| {
                ControlError::new(
                    "runtime_spawn_failed",
                    format!("无法启动共享核心 {}：{error}", executable.display()),
                )
            })?;
            stderr = spawned
                .stderr
                .take()
                .map(|pipe| BufReader::new(pipe).lines());
            child = Some(spawned);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ControlError::new(
                "runtime_start_failed",
                "共享核心未能在 15 秒内接受持有者连接；可直接运行 dg-lab-link-core --json 查看错误",
            ));
        }
        if let Some(reader) = &mut stderr {
            let startup = tokio::select! {
                result = reader.next_line() => Some(result),
                _ = tokio::time::sleep(Duration::from_millis(100)) => None,
            };
            if let Some(result) = startup {
                match result {
                    Ok(Some(line)) => {
                        if let Ok(error) = serde_json::from_str::<ControlError>(&line) {
                            if error.code != "core_already_running" {
                                return Err(error);
                            }
                            // Another launcher won the process lock. Wait for it;
                            // if it is shutting down, retry after its lock is released.
                            stderr = None;
                            child = None;
                        }
                    }
                    Ok(None) => {
                        let status = child
                            .as_mut()
                            .and_then(|child| child.try_wait().ok().flatten());
                        return Err(ControlError::new(
                            "runtime_start_failed",
                            format!("共享核心在握手前退出：{status:?}"),
                        ));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        } else {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn core_lock_available(directory: &Path) -> Result<bool, ControlError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("core.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

fn restore_source_configs(
    update: &mut HubSnapshot,
    previous: &HubSnapshot,
    configs: Option<BTreeMap<String, Arc<Value>>>,
) {
    for source in &mut update.sources {
        source.config = configs
            .as_ref()
            .and_then(|configs| configs.get(&source.id))
            .cloned()
            .or_else(|| {
                previous
                    .sources
                    .iter()
                    .find(|old| old.id == source.id)
                    .map(|old| old.config.clone())
            })
            .unwrap_or_else(|| Arc::new(serde_json::json!({})));
    }
}

fn unavailable() -> ControlError {
    ControlError::new("runtime_unavailable", "共享核心未运行或本机连接已关闭")
}

fn invalid_response(error: serde_json::Error) -> ControlError {
    ControlError::new("runtime_protocol_error", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_snapshots_restore_configuration_and_apply_catalogue_updates() {
        let directory = tempfile::tempdir().unwrap();
        let (service, _runtime) = dg_lab_link_core::ControlService::create(
            directory.path().to_owned(),
            "ws://127.0.0.1:1/v4".into(),
        )
        .unwrap();
        let mut previous = service.snapshot();
        previous.sources[0].config = Arc::new(serde_json::json!({"gain":3}));
        let mut update = previous.clone();
        update.sources[0].config = Arc::new(serde_json::json!({}));
        restore_source_configs(&mut update, &previous, None);
        assert!(Arc::ptr_eq(
            &previous.sources[0].config,
            &update.sources[0].config
        ));
        let changed = Arc::new(serde_json::json!({"gain":7}));
        restore_source_configs(
            &mut update,
            &previous,
            Some(BTreeMap::from([(
                previous.sources[0].id.clone(),
                changed.clone(),
            )])),
        );
        assert!(Arc::ptr_eq(&update.sources[0].config, &changed));
        update.sources.clear();
        restore_source_configs(&mut update, &previous, None);
        assert!(update.sources.is_empty());
    }

    #[tokio::test]
    async fn cancelled_request_does_not_leak_pending_capacity() {
        let directory = std::env::temp_dir().join(format!("dglab-runtime-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let (service, _runtime) = dg_lab_link_core::ControlService::create(
            directory.clone(),
            "ws://127.0.0.1:1/v4".to_owned(),
        )
        .unwrap();
        let (requests, mut requests_rx) = mpsc::channel(32);
        let (safety_requests, _safety_rx) = mpsc::channel(8);
        let (snapshot, _) = watch::channel(service.snapshot());
        let pending = Arc::new(std::sync::Mutex::new(BTreeMap::new()));
        let client = Client {
            inner: Arc::new(ClientInner {
                holder_id: "test".to_owned(),
                requests,
                safety_requests,
                pending: pending.clone(),
                next_id: AtomicU64::new(1),
                snapshot,
                closed: CancellationToken::new(),
                stop_epoch: Arc::new(AtomicU64::new(0)),
                core_epoch: Arc::new(AtomicU64::new(0)),
            }),
        };
        let request =
            tokio::spawn(async move { client.call(ControlCommand::GetHubSnapshot).await });
        requests_rx.recv().await.unwrap();
        assert_eq!(pending.lock().unwrap().len(), 1);
        request.abort();
        let _ = request.await;
        assert!(pending.lock().unwrap().is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
