use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize};

use futures_util::future::BoxFuture;
use futures_util::stream;

use super::backend::{GattBackend, GattInfo};
use super::*;

struct FakeGatt {
    writes: Mutex<Vec<Vec<u8>>>,
    notifications: mpsc::Sender<GattNotification>,
    notify_receiver: Mutex<Option<mpsc::Receiver<GattNotification>>>,
    strength: Mutex<[u8; 2]>,
    strength_ack: AtomicBool,
    extensions_ack: AtomicBool,
    fail_bf: AtomicUsize,
    write_delay_ms: AtomicUsize,
    disconnected: AtomicBool,
}

impl FakeGatt {
    fn new() -> Arc<Self> {
        let (notifications, receiver) = mpsc::channel(128);
        Arc::new(Self {
            writes: Mutex::new(Vec::new()),
            notifications,
            notify_receiver: Mutex::new(Some(receiver)),
            strength: Mutex::new([30, 40]),
            strength_ack: AtomicBool::new(true),
            extensions_ack: AtomicBool::new(true),
            fail_bf: AtomicUsize::new(0),
            write_delay_ms: AtomicUsize::new(0),
            disconnected: AtomicBool::new(false),
        })
    }

    fn writes(&self) -> Vec<Vec<u8>> {
        self.writes.lock().unwrap().clone()
    }
    fn clear(&self) {
        self.writes.lock().unwrap().clear();
    }
}

impl GattBackend for FakeGatt {
    fn write<'a>(&'a self, value: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>> {
        Box::pin(async move {
            let delay = self.write_delay_ms.load(Ordering::Acquire);
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay as u64)).await;
            }
            if value[0] == 0xBF
                && self
                    .fail_bf
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_sub(1))
                    .is_ok()
            {
                return Err(TransportError::new("fake_write_failed", "模拟 BF 写入失败"));
            }
            self.writes.lock().unwrap().push(value.to_vec());
            match value[0] {
                0xB0 => {
                    let mut strength = self.strength.lock().unwrap();
                    for index in 0..2 {
                        let shift = if index == 0 { 2 } else { 0 };
                        let mode = (value[1] >> shift) & 3;
                        strength[index] = match mode {
                            1 => strength[index].saturating_add(value[2 + index]).min(200),
                            2 => strength[index].saturating_sub(value[2 + index]),
                            3 => value[2 + index],
                            _ => strength[index],
                        };
                    }
                    let sequence = value[1] >> 4;
                    if sequence != 0 && self.strength_ack.load(Ordering::Acquire) {
                        let _ = self.notifications.try_send(GattNotification::Message(vec![
                            0xB1,
                            sequence,
                            strength[0],
                            strength[1],
                        ]));
                    }
                }
                0xBC if self.extensions_ack.load(Ordering::Acquire) => {
                    let _ = self.notifications.try_send(GattNotification::Message(
                        protocol::STANDARD_MODE_ACK.to_vec(),
                    ));
                }
                0xC4 if self.extensions_ack.load(Ordering::Acquire) => {
                    let _ = self
                        .notifications
                        .try_send(GattNotification::Message(vec![0xC4, value[1]]));
                }
                _ => {}
            }
            Ok(())
        })
    }

    fn disconnect(&self) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(async move {
            self.disconnected.store(true, Ordering::Release);
            Ok(())
        })
    }
}

impl GattFactory for Arc<FakeGatt> {
    fn open<'a>(
        &'a self,
        _device_id: &'a str,
    ) -> BoxFuture<'a, Result<GattConnection, TransportError>> {
        Box::pin(async move {
            let receiver = self
                .notify_receiver
                .lock()
                .unwrap()
                .take()
                .expect("one fake connect");
            let notifications = stream::unfold(receiver, |mut receiver| async {
                receiver.recv().await.map(|value| (value, receiver))
            })
            .boxed();
            Ok(GattConnection {
                backend: self.clone(),
                notifications,
                info: GattInfo {
                    name: "47L121000".to_owned(),
                    battery: Some(90),
                    firmware: Some(10),
                    battery_notifications: true,
                    load_notifications: true,
                },
            })
        })
    }
}

async fn setup() -> (
    Arc<FakeGatt>,
    SessionHandle,
    JoinHandle<()>,
    mpsc::Receiver<SessionEvent>,
) {
    let fake = FakeGatt::new();
    let (events, receiver) = mpsc::channel(512);
    let (handle, task) =
        spawn_with_factory("test-device".to_owned(), events, Arc::new(fake.clone()));
    handle
        .connect("test-device".to_owned(), Some(BleParameters::default()), 0)
        .await
        .unwrap();
    (fake, handle, task, receiver)
}

async fn operation_finished(
    receiver: &mut mpsc::Receiver<SessionEvent>,
    request_id: &str,
) -> (Result<(), TransportError>, bool) {
    timeout(Duration::from_secs(3), async {
        loop {
            if let Some(SessionEvent::OperationFinished {
                request_id: id,
                result,
                confirmed,
                ..
            }) = receiver.recv().await
                && id == request_id
            {
                return (result, confirmed);
            }
        }
    })
    .await
    .expect("operation completed")
}

async fn shutdown(handle: SessionHandle, task: JoinHandle<()>) {
    handle.shutdown_now();
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn initialization_orders_query_bf_and_optional_extensions_without_zeroing_base_strength() {
    let (fake, handle, task, mut receiver) = setup().await;
    let writes = fake.writes();
    assert_eq!(writes[0], protocol::query_strength(1));
    assert_eq!(writes[1], [0xBF, 100, 100, 160, 160, 0, 0]);
    assert_eq!(writes[2], protocol::STANDARD_MODE);
    assert_eq!(writes[3], [0xC4, 10]);
    assert_eq!(*fake.strength.lock().unwrap(), [30, 40]);
    let mut ready = None;
    while let Ok(event) = receiver.try_recv() {
        if let SessionEvent::Device { device, .. } = event
            && device.initialization == InitializationState::Ready
        {
            ready = Some(device);
        }
    }
    let ready = ready.unwrap();
    assert_eq!((ready.intensity_a, ready.intensity_b), (30, 40));
    assert_eq!(ready.configuration_status.as_deref(), Some("sent"));
    assert!(ready.capabilities.standard_mode && ready.capabilities.wheel_protection);
    shutdown(handle, task).await;
}

#[tokio::test]
async fn normal_waves_never_repeat_relative_delta_and_b1_confirms_only_matching_request() {
    let (fake, handle, task, mut receiver) = setup().await;
    fake.clear();
    handle
        .try_send(
            DeviceOperation::AdjustIntensity {
                request_id: "adjust".to_owned(),
                slot_id: SLOT_ID.to_owned(),
                channel: Channel::A,
                delta: 7,
            },
            0,
        )
        .unwrap();
    assert_eq!(
        operation_finished(&mut receiver, "adjust").await,
        (Ok(()), true)
    );
    for id in ["wave1", "wave2"] {
        tokio::time::sleep(Duration::from_millis(20)).await;
        handle
            .try_send(
                DeviceOperation::Wave {
                    request_id: id.to_owned(),
                    slot_id: SLOT_ID.to_owned(),
                    channel: Channel::B,
                    frame: WaveFrame::repeat(crate::model::WaveSample::new(30, 50).unwrap()),
                },
                0,
            )
            .unwrap();
        assert_eq!(operation_finished(&mut receiver, id).await, (Ok(()), false));
    }
    let writes = fake.writes();
    assert_eq!(
        writes
            .iter()
            .filter(|v| v[0] == 0xB0 && v[1] & 15 != 0)
            .count(),
        1
    );
    assert_eq!(*fake.strength.lock().unwrap(), [37, 40]);
    shutdown(handle, task).await;
}

#[tokio::test]
async fn zero_stop_bypasses_b1_and_stale_wave_queue_and_late_ack() {
    let (fake, handle, task, mut receiver) = setup().await;
    fake.clear();
    fake.strength_ack.store(false, Ordering::Release);
    handle
        .try_send(
            DeviceOperation::AdjustIntensity {
                request_id: "waiting".to_owned(),
                slot_id: SLOT_ID.to_owned(),
                channel: Channel::A,
                delta: 9,
            },
            0,
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(130)).await;
    let sequence = fake.writes()[0][1] >> 4;
    for index in 0..80 {
        handle
            .try_send(
                DeviceOperation::Wave {
                    request_id: format!("old-{index}"),
                    slot_id: SLOT_ID.to_owned(),
                    channel: Channel::A,
                    frame: WaveFrame::repeat(crate::model::WaveSample::new(50, 100).unwrap()),
                },
                0,
            )
            .unwrap();
    }
    timeout(
        Duration::from_millis(300),
        handle.stop(SLOT_ID.to_owned(), None, true, 1),
    )
    .await
    .unwrap()
    .unwrap();
    let before_late = fake.writes().len();
    fake.notifications
        .try_send(GattNotification::Message(vec![0xB1, sequence, 39, 40]))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let writes = fake.writes();
    let stop = writes
        .iter()
        .rposition(|v| v[0] == 0xB0 && v[1] & 15 == 15)
        .unwrap();
    assert!(stop < before_late);
    assert!(
        writes
            .iter()
            .skip(stop + 1)
            .all(|v| v[0] != 0xB0 || v[1] & 15 == 0)
    );
    assert_eq!(*fake.strength.lock().unwrap(), [0, 0]);
    while let Ok(event) = receiver.try_recv() {
        if let SessionEvent::Device { device, .. } = event {
            // Previous snapshots remain in the observer queue; no post-stop late state is emitted.
            assert_ne!(device.intensity_a, 39);
        }
    }
    shutdown(handle, task).await;
}

#[tokio::test]
async fn channel_clear_preserves_other_channel_strength_and_wave() {
    let (fake, handle, task, mut receiver) = setup().await;
    fake.clear();
    let frame = WaveFrame::repeat(crate::model::WaveSample::new(70, 40).unwrap());
    for channel in Channel::ALL {
        let id = channel.to_string();
        handle
            .try_send(
                DeviceOperation::Wave {
                    request_id: id,
                    slot_id: SLOT_ID.to_owned(),
                    channel,
                    frame,
                },
                0,
            )
            .unwrap();
    }
    operation_finished(&mut receiver, "A").await.0.unwrap();
    handle
        .stop(SLOT_ID.to_owned(), Some(Channel::A), false, 0)
        .await
        .unwrap();
    let writes = fake.writes();
    let clear = writes.last().unwrap();
    assert_eq!(&clear[4..12], &[0, 0, 0, 0, 0, 0, 0, 101]);
    assert_eq!(&clear[12..], &[70, 70, 70, 70, 40, 40, 40, 40]);
    assert_eq!(*fake.strength.lock().unwrap(), [30, 40]);
    shutdown(handle, task).await;
}

#[tokio::test]
async fn failed_bf_configuration_rolls_back_and_remains_unconfirmed() {
    let (fake, handle, task, _) = setup().await;
    fake.clear();
    fake.fail_bf.store(1, Ordering::Release);
    let changed = BleParameters {
        max_strength_a: 120,
        ..BleParameters::default()
    };
    let error = handle.configure(changed, 0).await.unwrap_err();
    assert_eq!(error.code, "fake_write_failed");
    assert_eq!(
        fake.writes().last().unwrap(),
        &[0xBF, 100, 100, 160, 160, 0, 0]
    );
    shutdown(handle, task).await;
}

#[tokio::test]
async fn init_extensions_timeout_is_optional_and_stop_interrupts_initialization() {
    let fake = FakeGatt::new();
    fake.extensions_ack.store(false, Ordering::Release);
    let (events, _receiver) = mpsc::channel(512);
    let (handle, task) =
        spawn_with_factory("test-device".to_owned(), events, Arc::new(fake.clone()));
    let connecting = tokio::spawn({
        let handle = handle.clone();
        async move { handle.connect("test-device".to_owned(), None, 0).await }
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    timeout(
        Duration::from_millis(300),
        handle.stop(SLOT_ID.to_owned(), None, true, 1),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(connecting.await.unwrap().is_err());
    shutdown(handle, task).await;
}

#[tokio::test]
async fn slow_observer_does_not_block_safety() {
    let fake = FakeGatt::new();
    let (events, _receiver) = mpsc::channel(1);
    let (handle, task) =
        spawn_with_factory("test-device".to_owned(), events, Arc::new(fake.clone()));
    handle
        .connect("test-device".to_owned(), None, 0)
        .await
        .unwrap();
    timeout(
        Duration::from_millis(300),
        handle.stop(SLOT_ID.to_owned(), None, true, 1),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(*fake.strength.lock().unwrap(), [0, 0]);
    shutdown(handle, task).await;
}

#[tokio::test]
async fn physical_wheel_feedback_is_the_current_strength() {
    let (fake, handle, task, mut receiver) = setup().await;
    while receiver.try_recv().is_ok() {}
    fake.notifications
        .try_send(GattNotification::Message(vec![0xB1, 0, 52, 81]))
        .unwrap();
    let device = timeout(Duration::from_secs(1), async {
        loop {
            if let Some(SessionEvent::Device { device, .. }) = receiver.recv().await {
                return device;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!((device.intensity_a, device.intensity_b), (52, 81));
    shutdown(handle, task).await;
}

#[tokio::test]
async fn optional_extensions_can_timeout_and_device_still_becomes_ready() {
    let fake = FakeGatt::new();
    fake.extensions_ack.store(false, Ordering::Release);
    let (events, mut receiver) = mpsc::channel(512);
    let (handle, task) =
        spawn_with_factory("test-device".to_owned(), events, Arc::new(fake.clone()));
    handle
        .connect("test-device".to_owned(), None, 0)
        .await
        .unwrap();
    let mut ready = None;
    while let Ok(event) = receiver.try_recv() {
        if let SessionEvent::Device { device, .. } = event
            && device.initialization == InitializationState::Ready
        {
            ready = Some(device);
        }
    }
    let ready = ready.expect("ready after optional extensions");
    assert!(!ready.capabilities.standard_mode && !ready.capabilities.wheel_protection);
    assert_eq!(ready.configuration_status.as_deref(), Some("sent"));
    shutdown(handle, task).await;
}

#[tokio::test]
async fn missing_strength_ack_stops_later_adjustments_without_retry() {
    let (fake, handle, task, mut receiver) = setup().await;
    fake.clear();
    fake.strength_ack.store(false, Ordering::Release);
    handle
        .try_send(
            DeviceOperation::AdjustIntensity {
                request_id: "timeout".to_owned(),
                slot_id: SLOT_ID.to_owned(),
                channel: Channel::A,
                delta: 5,
            },
            0,
        )
        .unwrap();
    let (result, confirmed) = operation_finished(&mut receiver, "timeout").await;
    assert_eq!(result.unwrap_err().code, "bluetooth_strength_timeout");
    assert!(!confirmed);
    handle
        .try_send(
            DeviceOperation::AdjustIntensity {
                request_id: "blocked".to_owned(),
                slot_id: SLOT_ID.to_owned(),
                channel: Channel::A,
                delta: 5,
            },
            0,
        )
        .unwrap();
    assert_eq!(
        operation_finished(&mut receiver, "blocked")
            .await
            .0
            .unwrap_err()
            .code,
        "bluetooth_strength_blocked"
    );
    assert_eq!(
        fake.writes()
            .iter()
            .filter(|v| v[0] == 0xB0 && v[1] & 15 != 0)
            .count(),
        1
    );
    shutdown(handle, task).await;
}

#[tokio::test]
async fn slow_native_write_is_bounded_before_safety_and_no_old_wave_follows_zero() {
    let (fake, handle, task, _) = setup().await;
    fake.clear();
    fake.write_delay_ms.store(300, Ordering::Release);
    handle
        .try_send(
            DeviceOperation::Wave {
                request_id: "slow".to_owned(),
                slot_id: SLOT_ID.to_owned(),
                channel: Channel::A,
                frame: WaveFrame::repeat(crate::model::WaveSample::new(50, 80).unwrap()),
            },
            0,
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    timeout(
        Duration::from_millis(800),
        handle.stop(SLOT_ID.to_owned(), None, true, 1),
    )
    .await
    .unwrap()
    .unwrap();
    fake.write_delay_ms.store(0, Ordering::Release);
    tokio::time::sleep(Duration::from_millis(120)).await;
    let writes = fake.writes();
    let zero = writes
        .iter()
        .rposition(|v| v[0] == 0xB0 && v[1] & 15 == 15)
        .unwrap();
    assert_eq!(zero, writes.len() - 1);
    shutdown(handle, task).await;
}

#[tokio::test]
async fn dual_channel_frames_are_merged_and_native_packets_do_not_accelerate() {
    let (fake, handle, task, mut receiver) = setup().await;
    fake.clear();
    let start = Instant::now();
    for frame_index in 0..3 {
        if frame_index != 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        for channel in Channel::ALL {
            handle
                .try_send(
                    DeviceOperation::Wave {
                        request_id: format!("{frame_index}-{channel}"),
                        slot_id: SLOT_ID.to_owned(),
                        channel,
                        frame: WaveFrame::repeat(crate::model::WaveSample::new(50, 30).unwrap()),
                    },
                    0,
                )
                .unwrap();
        }
        operation_finished(&mut receiver, &format!("{frame_index}-A"))
            .await
            .0
            .unwrap();
    }
    assert!(start.elapsed() >= Duration::from_millis(200));
    let packets = fake.writes();
    assert_eq!(packets.len(), 3);
    for packet in packets {
        assert_eq!(&packet[4..12], &packet[12..]);
    }
    shutdown(handle, task).await;
}

#[tokio::test]
async fn advancing_global_floor_on_idle_device_accepts_new_generation() {
    let (fake, handle, task, mut receiver) = setup().await;
    handle.invalidate_operations(5);
    handle
        .try_send(
            DeviceOperation::AdjustIntensity {
                request_id: "new-generation".to_owned(),
                slot_id: SLOT_ID.to_owned(),
                channel: Channel::B,
                delta: 2,
            },
            5,
        )
        .unwrap();
    assert_eq!(
        operation_finished(&mut receiver, "new-generation").await,
        (Ok(()), true)
    );
    assert_eq!(*fake.strength.lock().unwrap(), [30, 42]);
    shutdown(handle, task).await;
}

#[tokio::test]
async fn observer_recovers_latest_ready_and_all_bounded_operation_completions() {
    let fake = FakeGatt::new();
    let (events, mut receiver) = mpsc::channel(1);
    let (handle, task) =
        spawn_with_factory("test-device".to_owned(), events, Arc::new(fake.clone()));
    handle
        .connect("test-device".to_owned(), None, 0)
        .await
        .unwrap();
    for index in 0..5 {
        handle
            .try_send(
                DeviceOperation::AdjustIntensity {
                    request_id: format!("backpressure-{index}"),
                    slot_id: SLOT_ID.to_owned(),
                    channel: Channel::B,
                    delta: 1,
                },
                0,
            )
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(450)).await;
    timeout(
        Duration::from_millis(300),
        handle.stop(SLOT_ID.to_owned(), None, true, 1),
    )
    .await
    .unwrap()
    .unwrap();
    let (finished, ready) = timeout(Duration::from_secs(2), async {
        let mut finished = std::collections::BTreeSet::new();
        let mut ready = false;
        loop {
            match receiver.recv().await {
                Some(SessionEvent::OperationFinished { request_id, .. }) => {
                    finished.insert(request_id);
                }
                Some(SessionEvent::Device { device, .. }) => {
                    ready = device.initialization == InitializationState::Ready
                        && device.intensity_a == 0
                        && device.intensity_b == 0;
                }
                _ => {}
            }
            if finished.len() == 5 && ready {
                return (finished, ready);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(finished.len(), 5);
    assert!(ready);
    shutdown(handle, task).await;
}
