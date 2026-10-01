use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Notify, mpsc};
use tokio::time::{Duration, timeout};

use super::SessionEvent;

const MAX_FINISHED: usize = 512;

#[derive(Default)]
struct Outbox {
    finished: VecDeque<SessionEvent>,
    connection: Option<SessionEvent>,
    device: Option<SessionEvent>,
    removed: Option<SessionEvent>,
    log: Option<SessionEvent>,
}

struct State {
    queue: Mutex<Outbox>,
    available: Notify,
    closed: AtomicBool,
    overflowed: AtomicBool,
}

/// Actor snapshots coalesce while ordered operation completions stay bounded.
/// The observer's socket or queue can never hold native safety I/O.
pub struct EventSink(Arc<State>);

impl EventSink {
    pub fn new(sender: mpsc::Sender<SessionEvent>) -> Self {
        let state = Arc::new(State {
            queue: Mutex::new(Outbox::default()),
            available: Notify::new(),
            closed: AtomicBool::new(false),
            overflowed: AtomicBool::new(false),
        });
        let delivery = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                let event = {
                    let mut queue = delivery.queue.lock().unwrap_or_else(|e| e.into_inner());
                    queue
                        .finished
                        .pop_front()
                        .or_else(|| queue.removed.take())
                        .or_else(|| queue.connection.take())
                        .or_else(|| queue.device.take())
                        .or_else(|| queue.log.take())
                };
                let Some(event) = event else {
                    if delivery.closed.load(Ordering::Acquire) {
                        break;
                    }
                    delivery.available.notified().await;
                    continue;
                };
                if delivery.closed.load(Ordering::Acquire) {
                    if !matches!(
                        timeout(Duration::from_secs(1), sender.send(event)).await,
                        Ok(Ok(()))
                    ) {
                        break;
                    }
                } else if sender.send(event).await.is_err() {
                    break;
                }
            }
        });
        Self(state)
    }

    pub fn push(&self, event: SessionEvent) {
        let mut queue = self.0.queue.lock().unwrap_or_else(|e| e.into_inner());
        match &event {
            SessionEvent::OperationFinished { .. } => {
                if queue.finished.len() < MAX_FINISHED {
                    queue.finished.push_back(event);
                } else {
                    self.0.overflowed.store(true, Ordering::Release);
                }
            }
            SessionEvent::Connection(_) => queue.connection = Some(event),
            SessionEvent::Device { .. } => queue.device = Some(event),
            SessionEvent::Removed { .. } => {
                queue.device = None;
                queue.removed = Some(event);
            }
            SessionEvent::Log { .. } => queue.log = Some(event),
        }
        drop(queue);
        self.0.available.notify_one();
    }

    pub fn overflowed(&self) -> bool {
        self.0.overflowed.load(Ordering::Acquire)
    }
}

impl Drop for EventSink {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::Release);
        self.0.available.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::ChannelStatus;
    use crate::transport::{DeviceCapabilities, InitializationState, SessionDevice};

    fn device(initialization: InitializationState, intensity: u16) -> SessionEvent {
        SessionEvent::Device {
            connection_id: "test-connection".to_owned(),
            client_id: "test-client".to_owned(),
            device: SessionDevice {
                id: "test-device".to_owned(),
                slot_id: "test-slot".to_owned(),
                name: "test".to_owned(),
                device_type: "UNKNOWN".to_owned(),
                power: None,
                intensity_a: intensity,
                intensity_b: 0,
                intensity_limit_a: 100,
                intensity_limit_b: 100,
                channel_a_status: ChannelStatus::Idle,
                channel_b_status: ChannelStatus::Idle,
                initialization,
                capabilities: DeviceCapabilities::default(),
                ble_parameters: None,
                configuration_status: None,
            },
        }
    }

    #[tokio::test]
    async fn slow_observer_recovers_latest_ready_and_ordered_completions() {
        let (sender, mut observer) = mpsc::channel(1);
        let events = EventSink::new(sender);
        events.push(device(InitializationState::Initializing, 0));
        tokio::task::yield_now().await;
        for index in 0..3 {
            events.push(SessionEvent::OperationFinished {
                connection_id: "test-connection".to_owned(),
                client_id: "test-client".to_owned(),
                request_id: format!("operation-{index}"),
                result: Ok(()),
                confirmed: true,
            });
        }
        events.push(device(InitializationState::Ready, 12));
        // The observer is deliberately stalled while producers keep publishing.
        tokio::task::yield_now().await;
        assert!(!events.overflowed());
        let (completed, ready) = timeout(Duration::from_secs(1), async {
            let mut completed = Vec::new();
            let mut ready = false;
            while completed.len() < 3 || !ready {
                match observer.recv().await {
                    Some(SessionEvent::OperationFinished {
                        request_id,
                        result,
                        confirmed,
                        ..
                    }) => {
                        assert_eq!(result, Ok(()));
                        assert!(confirmed);
                        completed.push(request_id);
                    }
                    Some(SessionEvent::Device { device, .. }) => {
                        ready = device.initialization == InitializationState::Ready
                            && device.intensity_a == 12;
                    }
                    _ => {}
                }
            }
            (completed, ready)
        })
        .await
        .unwrap();
        assert_eq!(completed, ["operation-0", "operation-1", "operation-2"]);
        assert!(ready);
    }
}
