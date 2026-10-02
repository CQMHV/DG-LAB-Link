use super::*;
use crate::transport::{SessionCommand, SessionDevice, session_channel};

fn session_device() -> SessionDevice {
    SessionDevice {
        id: "fixture".to_owned(),
        slot_id: "slot".to_owned(),
        name: "Fixture".to_owned(),
        device_type: "UNKNOWN".to_owned(),
        power: None,
        intensity_a: 20,
        intensity_b: 30,
        intensity_limit_a: 100,
        intensity_limit_b: 100,
        channel_a_status: ChannelStatus::Ready,
        channel_b_status: ChannelStatus::Ready,
        initialization: InitializationState::Ready,
        capabilities: DeviceCapabilities {
            battery: false,
            load_status: false,
            ..DeviceCapabilities::default()
        },
        ble_parameters: None,
        configuration_status: None,
    }
}

async fn install(runtime: &mut HubRuntime, connection_id: &str) -> DeviceKey {
    let key = DeviceKey {
        connection_id: connection_id.to_owned(),
        client_id: "same-client".to_owned(),
        slot_id: "slot".to_owned(),
    };
    runtime
        .handle_session_event(SessionEvent::Device {
            connection_id: connection_id.to_owned(),
            client_id: key.client_id.clone(),
            device: session_device(),
        })
        .await;
    key
}

#[tokio::test]
async fn typed_devices_preserve_unknown_metadata_and_cancelled_wave_does_not_stop_other_channels() {
    let (_hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".to_owned());
    let v3 = install(&mut runtime, V3_CONNECTION_ID).await;
    let ble = install(&mut runtime, "ble:fixture").await;
    for key in [&v3, &ble] {
        runtime.output_devices.insert(key.clone());
    }
    runtime.snapshot.output.state = OutputState::Running;
    assert!(
        runtime
            .snapshot
            .devices
            .iter()
            .all(|d| d.power.is_none() && !d.capabilities.load_status)
    );
    let request_id = "cleared-a".to_owned();
    runtime.pending_wave_operations.insert(
        request_id.clone(),
        PendingWaveOperation {
            device: v3.clone(),
            channel: Channel::A,
            generation: 0,
            sent_at: Instant::now(),
        },
    );
    runtime
        .handle_session_event(SessionEvent::OperationFinished {
            connection_id: V3_CONNECTION_ID.to_owned(),
            client_id: v3.client_id.clone(),
            request_id,
            result: Err(TransportError::new("operation_cancelled", "cleared")),
            confirmed: false,
        })
        .await;
    assert_eq!(
        runtime.output_devices,
        BTreeSet::from([v3.clone(), ble.clone()])
    );
    assert!(runtime.pending_wave_operations.is_empty());
    runtime
        .handle_session_event(SessionEvent::Removed {
            connection_id: V3_CONNECTION_ID.to_owned(),
            client_id: v3.client_id,
        })
        .await;
    assert_eq!(runtime.output_devices, BTreeSet::from([ble]));
    assert_eq!(runtime.snapshot.output.state, OutputState::Running);
}

#[tokio::test]
async fn shutdown_stop_dispatches_to_other_sessions_before_a_slow_ack_and_invalidates_idle_connections()
 {
    let (_hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".to_owned());
    let v3 = install(&mut runtime, V3_CONNECTION_ID).await;
    let ble = install(&mut runtime, "ble:fixture").await;
    let (a, mut qa) = session_channel(8);
    let (b, mut qb) = session_channel(8);
    runtime.sessions.insert(v3.connection_id.clone(), a.clone());
    runtime
        .sessions
        .insert(ble.connection_id.clone(), b.clone());
    let ordinary = DeviceOperation::AdjustIntensity {
        request_id: "old".to_owned(),
        slot_id: "slot".to_owned(),
        channel: Channel::A,
        delta: 1,
    };
    a.try_send(ordinary.clone(), 0).unwrap();
    b.try_send(ordinary, 0).unwrap();
    let stop = runtime.send_stop_operations(true);
    let observe = async {
        let Some(SessionCommand::Stop {
            reply: slow,
            zero: true,
            ..
        }) = qa.safety.recv().await
        else {
            panic!("V3 stop")
        };
        // The other session receives zero before the first session acknowledges.
        let Some(SessionCommand::Stop {
            reply: fast,
            zero: true,
            ..
        }) = tokio::time::timeout(Duration::from_millis(100), qb.safety.recv())
            .await
            .unwrap()
        else {
            panic!("BLE stop")
        };
        fast.send(Ok(())).unwrap();
        slow.send(Ok(())).unwrap();
    };
    let (result, ()) = tokio::join!(stop, observe);
    result.unwrap();
    for (handle, queues) in [(a, &mut qa), (b, &mut qb)] {
        let SessionCommand::Operation(old) = queues.commands.try_recv().unwrap() else {
            panic!()
        };
        assert!(!handle.is_current(&old));
    }
}

#[tokio::test]
async fn a_late_error_for_an_old_request_cannot_stop_the_reconnected_device() {
    let (_hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".to_owned());
    let device = install(&mut runtime, "ble:fixture").await;
    runtime.output_devices.insert(device.clone());
    runtime.snapshot.output.state = OutputState::Running;
    runtime
        .handle_session_event(SessionEvent::OperationFinished {
            connection_id: device.connection_id,
            client_id: device.client_id,
            request_id: "discarded-before-reconnect".to_owned(),
            result: Err(TransportError::new("bluetooth_transport", "old failure")),
            confirmed: false,
        })
        .await;
    assert_eq!(runtime.output_devices.len(), 1);
    assert_eq!(runtime.snapshot.output.state, OutputState::Running);
}

#[tokio::test]
async fn all_three_transports_and_multiple_ble_devices_keep_distinct_addresses_and_fault_scope() {
    let (_hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".to_owned());
    let mut keys = Vec::new();
    for connection in [
        V4_CONNECTION_ID,
        V3_CONNECTION_ID,
        "ble:first",
        "ble:second",
    ] {
        let key = install(&mut runtime, connection).await;
        runtime.output_devices.insert(key.clone());
        keys.push(key);
    }
    runtime.snapshot.output.state = OutputState::Running;
    runtime.refresh_selected_device_snapshot();
    assert_eq!(runtime.snapshot.devices.len(), 4);
    assert_eq!(
        runtime
            .snapshot
            .devices
            .iter()
            .map(|device| device.control_id.clone())
            .collect::<BTreeSet<_>>()
            .len(),
        4
    );
    assert_eq!(
        runtime
            .snapshot
            .devices
            .iter()
            .map(|device| device.transport)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([TransportKind::WsV4, TransportKind::WsV3, TransportKind::Ble])
    );
    runtime.remove_connection_devices("ble:first");
    runtime.reconcile_connected_devices();
    runtime.refresh_selected_device_snapshot();
    assert_eq!(
        runtime.output_devices,
        BTreeSet::from([keys[0].clone(), keys[1].clone(), keys[3].clone()])
    );
    assert_eq!(runtime.snapshot.output.state, OutputState::Running);
    assert_eq!(runtime.snapshot.devices.len(), 3);
    runtime.remove_connection_devices(V3_CONNECTION_ID);
    runtime.reconcile_connected_devices();
    assert_eq!(
        runtime.output_devices,
        BTreeSet::from([keys[0].clone(), keys[3].clone()])
    );
}

#[tokio::test]
async fn events_from_an_old_native_session_cannot_recreate_or_remove_a_new_connection() {
    let (_hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".to_owned());
    let key = install(&mut runtime, "ble:fixture").await;
    runtime
        .session_identities
        .insert(key.connection_id.clone(), 2);
    runtime
        .handle_current_session_event(
            1,
            SessionEvent::Removed {
                connection_id: key.connection_id.clone(),
                client_id: key.client_id.clone(),
            },
        )
        .await;
    assert!(runtime.devices.contains_key(&key));
    let mut old = session_device();
    old.intensity_a = 99;
    runtime
        .handle_current_session_event(
            1,
            SessionEvent::Device {
                connection_id: key.connection_id.clone(),
                client_id: key.client_id.clone(),
                device: old,
            },
        )
        .await;
    assert_eq!(
        device_intensity_from_value(&runtime.devices[&key], Channel::A),
        Some(20)
    );
}

#[tokio::test]
async fn physical_wheel_sync_keeps_the_explicit_baseline_after_gui_focus_changes() {
    let (_hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".to_owned());
    let baseline = install(&mut runtime, "ble:baseline").await;
    let other = install(&mut runtime, "ble:other").await;
    let (a, _qa) = session_channel(8);
    let (b, mut qb) = session_channel(8);
    runtime.sessions.insert(baseline.connection_id.clone(), a);
    runtime.sessions.insert(other.connection_id.clone(), b);
    runtime
        .set_sync_all_devices_from(Some(baseline.control_id()), true)
        .unwrap();
    runtime.select_device(other.control_id()).await.unwrap();
    let mut wheel = session_device();
    wheel.intensity_a = 25;
    runtime
        .handle_session_event(SessionEvent::Device {
            connection_id: baseline.connection_id.clone(),
            client_id: baseline.client_id.clone(),
            device: wheel,
        })
        .await;
    let SessionCommand::Operation(operation) = qb.commands.try_recv().unwrap() else {
        panic!("synchronized wheel adjustment")
    };
    assert!(matches!(
        operation.operation,
        DeviceOperation::AdjustIntensity {
            channel: Channel::A,
            delta: 5,
            ..
        }
    ));
    assert_eq!(runtime.selected_device, Some(other));
    assert_eq!(runtime.sync_baseline_device, Some(baseline.clone()));
    runtime.remove_connection_devices(&baseline.connection_id);
    assert!(runtime.sync_baseline_device.is_none());
}

#[tokio::test]
async fn shutdown_zeroing_blocks_sync_until_both_channels_of_every_device_report_zero() {
    let (_hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".to_owned());
    let baseline = install(&mut runtime, "ble:baseline").await;
    let other = install(&mut runtime, "ble:other").await;
    let (a, mut qa) = session_channel(8);
    let (b, mut qb) = session_channel(8);
    runtime.sessions.insert(baseline.connection_id.clone(), a);
    runtime.sessions.insert(other.connection_id.clone(), b);
    runtime
        .set_sync_all_devices_from(Some(baseline.control_id()), true)
        .unwrap();
    let stop = runtime.send_stop_operations(true);
    let acknowledge = async {
        for queue in [&mut qa, &mut qb] {
            let Some(SessionCommand::Stop {
                reply, zero: true, ..
            }) = queue.safety.recv().await
            else {
                panic!("shutdown zero")
            };
            reply.send(Ok(())).unwrap();
        }
    };
    let (result, ()) = tokio::join!(stop, acknowledge);
    result.unwrap();
    assert_eq!(runtime.shutdown_zero_pending.len(), 2);
    runtime.devices.get_mut(&other).unwrap()["props"]["intensityA"] = json!(0);
    runtime.devices.get_mut(&other).unwrap()["props"]["intensityB"] = json!(0);
    runtime.reconcile_intensity_lock();
    assert_eq!(
        runtime.shutdown_zero_pending,
        BTreeSet::from([baseline.clone()])
    );
    assert!(qa.commands.try_recv().is_err());
    assert!(qb.commands.try_recv().is_err());
    assert!(matches!(
        runtime.start_output(&baseline.control_id()),
        Err(HubError::QueueBusy)
    ));
    assert!(matches!(
        runtime.adjust_device_intensity(Some(&other.control_id()), Channel::A, 1),
        Err(HubError::QueueBusy)
    ));
    runtime.devices.get_mut(&baseline).unwrap()["props"]["intensityA"] = json!(0);
    runtime.reconcile_intensity_lock();
    assert!(!runtime.shutdown_zero_pending.is_empty());
    runtime.devices.get_mut(&baseline).unwrap()["props"]["intensityB"] = json!(0);
    runtime.reconcile_intensity_lock();
    assert!(runtime.shutdown_zero_pending.is_empty());
    assert!(qa.commands.try_recv().is_err());
    assert!(qb.commands.try_recv().is_err());
}
