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
    runtime.refresh_device_snapshots();
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
    runtime.refresh_device_snapshots();
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
async fn command_epoch_watch_is_shared_monotonic_and_enqueue_does_not_revoke_twice() {
    let (hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".into());
    let mut epochs = hub.subscribe_command_epoch();
    assert_eq!(hub.accept_command(false), 0);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let hub = &hub;
            scope.spawn(move || {
                for _ in 0..16 {
                    hub.accept_command(true);
                }
            });
        }
    });
    epochs.changed().await.unwrap();
    assert_eq!(*epochs.borrow_and_update(), 128);
    assert_eq!(hub.command_epoch(), 128);
    assert_eq!(runtime.safety_epoch.load(Ordering::Acquire), 128);
    let device = install(&mut runtime, V3_CONNECTION_ID).await;
    let accepted = hub.accept_command(true);
    let client = hub.clone();
    let request =
        tokio::spawn(async move { client.enqueue_stop_output(device.control_id()).await });
    let command = runtime.safety_commands.recv().await.unwrap();
    runtime.handle_safety_command(command).await;
    request.await.unwrap().unwrap();
    assert_eq!(hub.command_epoch(), accepted);
    hub.shutdown_now();
    epochs.changed().await.unwrap();
    assert_eq!(*epochs.borrow(), accepted + 1);
}

#[tokio::test]
async fn old_v4_session_events_cannot_change_a_new_controller_or_its_devices() {
    let (_hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".into());
    super::tests::install_test_device(&mut runtime, "app", "slot", 20);
    runtime.v4_session_generation = Some(2);
    runtime.v4_connection.controller_id = Some("new-controller".into());
    runtime.v4_connection.state = ConnectionState::Connected;
    let current = runtime.devices.keys().next().unwrap().clone();
    for event in [
        RelayEvent::Hello {
            controller_id: "old-controller".into(),
        },
        RelayEvent::ClientAttached {
            client_id: "ghost-app".into(),
        },
        RelayEvent::ClientDisconnected {
            client_id: "app".into(),
        },
        RelayEvent::Message {
            client_id: "app".into(),
            data: json!({"t":"event","m":"device.update","data":{"s":"slot","iA":99}}),
        },
        RelayEvent::Disconnected {
            reason: "late close".into(),
            retryable: false,
        },
    ] {
        runtime
            .handle_current_relay_event(RelaySessionEvent {
                generation: 1,
                event,
            })
            .await;
    }
    assert_eq!(
        runtime.v4_connection.controller_id.as_deref(),
        Some("new-controller")
    );
    assert!(runtime.devices.contains_key(&current));
    assert!(!runtime.apps.contains("ghost-app"));
    assert_eq!(
        device_intensity_from_value(&runtime.devices[&current], Channel::A),
        Some(20)
    );
    runtime
        .handle_current_relay_event(RelaySessionEvent {
            generation: 2,
            event: RelayEvent::ClientDisconnected {
                client_id: "app".into(),
            },
        })
        .await;
    assert!(runtime.devices.is_empty());
}

#[tokio::test]
async fn disconnect_latches_only_owned_devices_and_slow_ack_does_not_block_other_output() {
    let (hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".into());
    runtime
        .set_default_source(Some(FIXED_WAVEFORM_SOURCE_ID.into()))
        .unwrap();
    let a = install(&mut runtime, V3_CONNECTION_ID).await;
    let b = install(&mut runtime, "ble:fixture").await;
    let (ha, mut qa) = session_channel(8);
    let (hb, mut qb) = session_channel(8);
    runtime.sessions.insert(a.connection_id.clone(), ha.clone());
    runtime.sessions.insert(b.connection_id.clone(), hb);
    runtime
        .session_identities
        .insert(a.connection_id.clone(), 1);
    for device in [&a, &b] {
        runtime.start_output(&device.control_id()).unwrap();
    }
    ha.try_send(
        DeviceOperation::Wave {
            request_id: "old-a".into(),
            slot_id: a.slot_id.clone(),
            channel: Channel::A,
            frame: WaveFrame::silent(),
        },
        0,
    )
    .unwrap();
    let (reply, response) = oneshot::channel();
    tokio::time::timeout(
        Duration::from_millis(100),
        runtime.handle_safety_command(HubSafetyCommand::DisconnectConnection {
            connection_id: a.connection_id.clone(),
            reply,
        }),
    )
    .await
    .unwrap();
    assert!(!runtime.devices.contains_key(&a));
    assert_eq!(runtime.output_devices, BTreeSet::from([b.clone()]));
    assert!(
        runtime.device_session(&a).is_ok(),
        "shutdown retains the captured physical link"
    );
    let SessionCommand::Operation(old) = qa.commands.try_recv().unwrap() else {
        panic!("old wave")
    };
    assert!(!ha.is_current(&old));
    let SessionCommand::Stop {
        reply: slow,
        zero: false,
        ..
    } = qa.safety.try_recv().unwrap()
    else {
        panic!("clear admitted")
    };
    runtime.output_tick().await;
    let SessionCommand::Operation(other) = qb.commands.try_recv().unwrap() else {
        panic!("other wave")
    };
    assert!(matches!(other.operation, DeviceOperation::Wave { .. }));
    assert_eq!(runtime.snapshot.output.state, OutputState::Running);
    runtime
        .handle_current_session_event(
            1,
            SessionEvent::Device {
                connection_id: a.connection_id.clone(),
                client_id: a.client_id.clone(),
                device: session_device(),
            },
        )
        .await;
    assert!(
        !runtime.devices.contains_key(&a),
        "late data cannot resurrect the disconnected device"
    );
    // A later stop invalidates activity, but does not cancel an admitted connection cleanup.
    hub.accept_command(true);
    slow.send(Ok(())).unwrap();
    let SessionCommand::Disconnect { reply } =
        tokio::time::timeout(Duration::from_secs(1), qa.safety.recv())
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("physical disconnect")
    };
    reply.send(Ok(())).unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(1), runtime.commands.recv())
        .await
        .unwrap()
        .unwrap();
    runtime.handle_command(completed).await;
    assert_eq!(response.await.unwrap(), Ok(()));
    assert!(ha.shutdown.is_cancelled());
    assert_eq!(runtime.output_devices, BTreeSet::from([b]));
}

#[tokio::test(start_paused = true)]
async fn connection_cleanup_timeout_terminates_captured_session_and_preserves_other_connections() {
    let (_hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".into());
    let a = install(&mut runtime, V3_CONNECTION_ID).await;
    let b = install(&mut runtime, "ble:fixture").await;
    let (ha, mut qa) = session_channel(8);
    let (hb, _qb) = session_channel(8);
    runtime.sessions.insert(a.connection_id.clone(), ha.clone());
    runtime.sessions.insert(b.connection_id.clone(), hb.clone());
    runtime.output_devices.extend([a.clone(), b.clone()]);
    runtime.snapshot.output.state = OutputState::Running;
    let (reply, response) = oneshot::channel();
    runtime.queue_connection_cleanup(
        &a.connection_id,
        None,
        0,
        Some(ConnectionReply::Unit(reply)),
    );
    let SessionCommand::Stop { reply: slow, .. } = qa.safety.recv().await.unwrap() else {
        panic!("clear")
    };
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(8)).await;
    let SessionCommand::Disconnect {
        reply: held_disconnect,
    } = qa.safety.recv().await.unwrap()
    else {
        panic!("disconnect attempted despite clear timeout")
    };
    tokio::time::advance(Duration::from_secs(2)).await;
    let completed = runtime.commands.recv().await.unwrap();
    runtime.handle_command(completed).await;
    assert_eq!(
        response.await.unwrap().unwrap_err().code(),
        "transport_timeout"
    );
    assert!(ha.shutdown.is_cancelled());
    assert!(!hb.shutdown.is_cancelled());
    assert_eq!(runtime.output_devices, BTreeSet::from([b]));
    assert!(runtime.pending_connection_cleanups.is_empty());
    assert!(slow.send(Ok(())).is_err());
    assert!(held_disconnect.send(Ok(())).is_err());
}

#[tokio::test]
async fn stop_during_pairing_refresh_prevents_its_continuation_from_reconnecting() {
    let (hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".into());
    let a = install(&mut runtime, V3_CONNECTION_ID).await;
    let (ha, mut qa) = session_channel(8);
    runtime.sessions.insert(a.connection_id.clone(), ha);
    let (reply, response) = oneshot::channel();
    runtime.queue_connection_cleanup(
        &a.connection_id,
        Some("ws://127.0.0.1:9000/".into()),
        hub.command_epoch(),
        Some(ConnectionReply::Value(reply)),
    );
    let SessionCommand::Stop { reply, .. } = qa.safety.recv().await.unwrap() else {
        panic!("clear")
    };
    hub.accept_command(true);
    reply.send(Ok(())).unwrap();
    let SessionCommand::Disconnect { reply } = qa.safety.recv().await.unwrap() else {
        panic!("disconnect")
    };
    reply.send(Ok(())).unwrap();
    let command = runtime.commands.recv().await.unwrap();
    runtime.handle_command(command).await;
    assert_eq!(response.await.unwrap(), Err(HubError::QueueBusy));
    assert!(!runtime.sessions.contains_key(&a.connection_id));
    assert!(!runtime.session_identities.contains_key(&a.connection_id));
}

#[tokio::test]
async fn v4_pairing_refresh_closes_old_actor_and_ignores_its_queued_events() {
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/v4", listener.local_addr().unwrap());
    let (observed, observation) = oneshot::channel();
    let relay = tokio::spawn(async move {
        for index in 1..=2 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(
                    json!({"type":"hello","clientId":format!("controller-{index}")})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            match socket.next().await {
                Some(Ok(Message::Close(_))) if index == 1 => {}
                Some(Ok(Message::Text(text))) if index == 2 => {
                    let value: Value = serde_json::from_str(text.as_ref()).unwrap();
                    assert_eq!(value["data"]["reqId"], "new-session-probe");
                    observed.send(()).unwrap();
                    while let Some(Ok(message)) = socket.next().await {
                        if matches!(message, Message::Close(_)) {
                            break;
                        }
                    }
                    return;
                }
                other => panic!("unexpected frame: {other:?}"),
            }
        }
    });
    let (hub, mut runtime) = create_hub(endpoint.clone());
    let (events, mut received) = mpsc::channel(32);
    runtime.relay_events = Some(events);
    // Refreshing an idle known WS connection is a valid explicit connect.
    let (reply, response) = oneshot::channel();
    runtime
        .handle_transport_action(
            TransportAction::RefreshPairing {
                connection_id: V4_CONNECTION_ID.into(),
            },
            hub.command_epoch(),
            reply,
        )
        .await;
    assert_eq!(response.await.unwrap(), Ok(Value::Null));
    let old = runtime.relay.clone().unwrap();
    let old_generation = runtime.v4_session_generation.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.v4_connection.controller_id.as_deref() != Some("controller-1") {
            runtime
                .handle_current_relay_event(received.recv().await.unwrap())
                .await;
        }
    })
    .await
    .unwrap();
    let (reply, response) = oneshot::channel();
    runtime
        .handle_transport_action(
            TransportAction::RefreshPairing {
                connection_id: V4_CONNECTION_ID.into(),
            },
            hub.command_epoch(),
            reply,
        )
        .await;
    assert!(
        runtime.relay.is_none(),
        "cleanup owns the old socket exclusively"
    );
    let completed = tokio::time::timeout(Duration::from_secs(2), runtime.commands.recv())
        .await
        .unwrap()
        .unwrap();
    runtime.handle_command(completed).await;
    assert_eq!(response.await.unwrap(), Ok(Value::Null));
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.v4_connection.controller_id.as_deref() != Some("controller-2") {
            runtime
                .handle_current_relay_event(received.recv().await.unwrap())
                .await;
        }
    })
    .await
    .unwrap();
    assert_ne!(runtime.v4_session_generation, Some(old_generation));
    assert_eq!(
        old.disconnect_session(Some(old_generation)).await,
        Err(RelayClientError::Stopped)
    );
    runtime
        .handle_current_relay_event(RelaySessionEvent {
            generation: old_generation,
            event: RelayEvent::Disconnected {
                reason: "late old close".into(),
                retryable: false,
            },
        })
        .await;
    assert_eq!(
        runtime.v4_connection.controller_id.as_deref(),
        Some("controller-2")
    );
    let current = runtime.relay.clone().unwrap();
    current
        .send_message("app", json!({"reqId":"new-session-probe"}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), observation)
        .await
        .unwrap()
        .unwrap();
    current.disconnect().await.unwrap();
    current.shutdown_now();
    tokio::time::timeout(Duration::from_secs(2), relay)
        .await
        .unwrap()
        .unwrap();
    for task in runtime.session_tasks {
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn explicit_v3_connection_replaces_the_previous_handle_and_event_identity() {
    let (_hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".into());
    let (old, _old_queue) = session_channel(8);
    runtime
        .sessions
        .insert(V3_CONNECTION_ID.into(), old.clone());
    runtime
        .session_identities
        .insert(V3_CONNECTION_ID.into(), 10);
    runtime.next_session_identity = 10;
    let (sender, _events) = mpsc::channel(32);
    runtime.session_events = Some(sender);
    let (reply, response) = oneshot::channel();
    runtime
        .handle_transport_action(
            TransportAction::Connect {
                transport: TransportKind::WsV3,
                endpoint: "invalid://fixture".into(),
            },
            0,
            reply,
        )
        .await;
    assert!(old.shutdown.is_cancelled());
    assert_eq!(runtime.session_identities.get(V3_CONNECTION_ID), Some(&11));
    // Commands are selected before event delivery. A second accepted connect
    // must still see the first one as pending and must not replace its actor.
    let (duplicate, duplicate_response) = oneshot::channel();
    runtime
        .handle_transport_action(
            TransportAction::Connect {
                transport: TransportKind::WsV3,
                endpoint: "invalid://duplicate".into(),
            },
            0,
            duplicate,
        )
        .await;
    assert_eq!(
        duplicate_response.await.unwrap().unwrap_err().code(),
        "already_connected"
    );
    assert_eq!(runtime.session_identities.get(V3_CONNECTION_ID), Some(&11));
    // Protocol validation fails without touching a real network or radio.
    assert_eq!(
        response.await.unwrap().unwrap_err().code(),
        "invalid_endpoint"
    );
    runtime
        .handle_current_session_event(
            10,
            SessionEvent::Device {
                connection_id: V3_CONNECTION_ID.into(),
                client_id: "old-app".into(),
                device: session_device(),
            },
        )
        .await;
    assert!(runtime.devices.is_empty());
    runtime.sessions[V3_CONNECTION_ID].shutdown_now();
    for task in runtime.session_tasks {
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn disconnect_known_idle_connections_is_idempotent_but_unknown_ids_fail() {
    let (hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".into());
    for id in [V4_CONNECTION_ID, V3_CONNECTION_ID, V4_CONNECTION_ID] {
        let (reply, response) = oneshot::channel();
        let accepted = hub.accept_command(true);
        runtime
            .handle_safety_command(HubSafetyCommand::DisconnectConnection {
                connection_id: id.into(),
                reply,
            })
            .await;
        assert_eq!(response.await.unwrap(), Ok(()));
        assert_eq!(hub.command_epoch(), accepted);
    }
    let (reply, response) = oneshot::channel();
    runtime
        .handle_safety_command(HubSafetyCommand::DisconnectConnection {
            connection_id: "ble:unknown".into(),
            reply,
        })
        .await;
    assert_eq!(response.await.unwrap(), Err(HubError::DeviceUnavailable));
}

#[tokio::test]
async fn final_cleanup_zeroes_a_v4_link_owned_by_pending_disconnect() {
    use futures_util::StreamExt;
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/v4", listener.local_addr().unwrap());
    let (observed, observation) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let mut zeroed = BTreeSet::new();
        let mut clears = 0;
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            let data = &frame["data"];
            assert_eq!(data["data"]["s"], "slot");
            match data["m"].as_str().unwrap() {
                "device.op.clear" => clears += 1,
                "device.op" => {
                    assert_eq!(data["data"]["t"], 7);
                    assert_eq!(data["data"]["v"], 0);
                    zeroed.insert(data["data"]["c"].as_u64().unwrap());
                }
                other => panic!("unexpected operation: {other}"),
            }
            if zeroed.len() == 2 {
                observed.send((clears, zeroed)).unwrap();
                while let Some(Ok(message)) = socket.next().await {
                    if matches!(message, Message::Close(_)) {
                        break;
                    }
                }
                return;
            }
        }
        panic!("closed before both zero operations");
    });
    let (_hub, mut runtime) = create_hub(endpoint.clone());
    let (events, _received) = mpsc::channel(16);
    let (relay, task) = spawn_relay_client(events, 16);
    relay.connect(endpoint).await.unwrap();
    runtime.relay = Some(relay.clone());
    runtime.v4_session_generation = Some(0);
    super::tests::install_test_device(&mut runtime, "app", "slot", 20);
    runtime.queue_connection_cleanup(V4_CONNECTION_ID, None, 0, None);
    assert!(runtime.devices.is_empty());
    assert!(runtime.relay.is_none());
    // Final cleanup takes ownership before the detached normal disconnect gets polled.
    runtime.pending_connection_cleanups[V4_CONNECTION_ID]
        .cancellation
        .cancel();
    runtime.send_stop_operations(true).await.unwrap();
    let (clears, zeroed) = tokio::time::timeout(Duration::from_secs(2), observation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(clears, 2);
    assert_eq!(zeroed, BTreeSet::from([0, 1]));
    relay.disconnect().await.unwrap();
    relay.shutdown_now();
    task.await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn physical_wheel_sync_keeps_the_explicit_baseline() {
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
    assert_eq!(runtime.sync_baseline_device, Some(baseline.clone()));
    runtime.remove_connection_devices(&baseline.connection_id);
    assert!(runtime.sync_baseline_device.is_none());
}

#[tokio::test]
async fn concurrent_ordinary_stops_latch_inactive_before_slow_device_ack() {
    let (hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".into());
    runtime
        .set_default_source(Some(FIXED_WAVEFORM_SOURCE_ID.into()))
        .unwrap();
    let a = install(&mut runtime, V3_CONNECTION_ID).await;
    let b = install(&mut runtime, "ble:fixture").await;
    let (ha, mut qa) = session_channel(8);
    let (hb, mut qb) = session_channel(8);
    runtime.sessions.insert(a.connection_id.clone(), ha.clone());
    runtime.sessions.insert(b.connection_id.clone(), hb.clone());
    for device in [&a, &b] {
        runtime.start_output(&device.control_id()).unwrap();
    }
    for handle in [&ha, &hb] {
        handle
            .try_send(
                DeviceOperation::Wave {
                    request_id: "old".into(),
                    slot_id: "slot".into(),
                    channel: Channel::A,
                    frame: WaveFrame::silent(),
                },
                0,
            )
            .unwrap();
    }
    let (reply_a, response_a) = oneshot::channel();
    let (reply_b, response_b) = oneshot::channel();
    hub.safety_commands
        .try_send(HubSafetyCommand::StopOutput {
            device_id: a.control_id(),
            reply: reply_a,
        })
        .unwrap();
    hub.safety_commands
        .try_send(HubSafetyCommand::StopOutput {
            device_id: b.control_id(),
            reply: reply_b,
        })
        .unwrap();
    for _ in 0..2 {
        let command = runtime.safety_commands.recv().await.unwrap();
        tokio::time::timeout(
            Duration::from_millis(100),
            runtime.handle_safety_command(command),
        )
        .await
        .unwrap();
    }
    assert!(runtime.output_devices.is_empty());
    assert!(
        runtime
            .device_source_bindings
            .values()
            .all(|binding| !binding.active)
    );
    assert!(
        runtime
            .snapshot
            .devices
            .iter()
            .all(|device| !device.output_active
                && device.intensity_a == 20
                && device.intensity_b == 30)
    );
    assert_eq!(
        runtime.start_output(&a.control_id()),
        Err(HubError::QueueBusy)
    );
    for (handle, queue) in [(&ha, &mut qa), (&hb, &mut qb)] {
        let SessionCommand::Operation(old) = queue.commands.try_recv().unwrap() else {
            panic!("old wave");
        };
        assert!(!handle.is_current(&old));
    }
    let SessionCommand::Stop {
        reply: slow,
        zero: false,
        ..
    } = qa.safety.try_recv().unwrap()
    else {
        panic!("A stop admitted");
    };
    let SessionCommand::Stop {
        reply: fast,
        zero: false,
        ..
    } = qb.safety.try_recv().unwrap()
    else {
        panic!("B stop admitted despite A awaiting ACK");
    };
    // Completing B cannot cancel A's admitted cleanup or re-enable either target.
    fast.send(Ok(())).unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(1), runtime.commands.recv())
        .await
        .unwrap()
        .unwrap();
    runtime.handle_command(completed).await;
    assert_eq!(response_b.await.unwrap(), Ok(()));
    assert!(runtime.pending_stops.contains_key(&a));
    assert!(runtime.output_devices.is_empty());
    slow.send(Err(TransportError::new(
        "write_failed",
        "simulated slow writer failure",
    )))
    .unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(1), runtime.commands.recv())
        .await
        .unwrap()
        .unwrap();
    runtime.handle_command(completed).await;
    assert!(response_a.await.unwrap().is_err());
    assert!(runtime.output_devices.is_empty());
    assert!(
        runtime
            .device_source_bindings
            .values()
            .all(|binding| !binding.active)
    );
    assert!(
        runtime
            .sources
            .values()
            .all(|source| source.snapshot.enabled)
    );
    runtime.output_tick().await;
    assert!(qa.commands.is_empty() && qb.commands.is_empty());
    runtime.start_output(&a.control_id()).unwrap();
    assert!(
        runtime
            .device_source_bindings
            .iter()
            .filter(|(key, _)| key.device == a)
            .all(|(_, binding)| binding.active)
    );
}

#[tokio::test]
async fn binding_config_cas_and_source_replacement_prevent_config_leak_and_aba() {
    let (_hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".into());
    runtime
        .set_default_source(Some(FIXED_WAVEFORM_SOURCE_ID.into()))
        .unwrap();
    let device = install(&mut runtime, V3_CONNECTION_ID).await;
    runtime
        .set_device_channel_source(device.control_id(), Channel::A, TOUCH_SOURCE_ID.into())
        .await
        .unwrap();
    let key = SourceBindingKey {
        device: device.clone(),
        channel: Channel::A,
    };
    let old_id = runtime.device_source_bindings[&key].id.clone();
    for (expected_revision, expected) in [(0, Ok(())), (0, Err(HubError::ConfigConflict))] {
        let (reply, response) = oneshot::channel();
        runtime
            .handle_command(HubCommand::SetPluginBindingConfig {
                source_id: TOUCH_SOURCE_ID.into(),
                binding_id: old_id.clone(),
                config: json!({"gain":7}),
                expected_revision,
                reply,
            })
            .await;
        assert_eq!(response.await.unwrap(), expected);
    }
    assert_eq!(runtime.device_source_bindings[&key].revision, 1);
    runtime
        .set_device_channel_source(device.control_id(), Channel::A, AUDIO_SOURCE_ID.into())
        .await
        .unwrap();
    assert_eq!(runtime.device_source_bindings[&key].config, json!({}));
    runtime
        .set_device_channel_source(device.control_id(), Channel::A, TOUCH_SOURCE_ID.into())
        .await
        .unwrap();
    let current = &runtime.device_source_bindings[&key];
    assert_ne!(current.id, old_id);
    assert_eq!(current.config, json!({}));
    assert_eq!(current.revision, 0);
    assert!(!current.active);
    let (reply, response) = oneshot::channel();
    runtime
        .handle_command(HubCommand::SetPluginBindingConfig {
            source_id: TOUCH_SOURCE_ID.into(),
            binding_id: old_id,
            config: json!({"gain":99}),
            expected_revision: 1,
            reply,
        })
        .await;
    assert_eq!(response.await.unwrap(), Err(HubError::ConfigConflict));
    assert_eq!(runtime.device_source_bindings[&key].config, json!({}));
}

#[tokio::test]
async fn stopping_one_v4_device_preserves_other_targets_wave_error_confirmation() {
    let (_hub, mut runtime) = create_hub("ws://127.0.0.1:9000/".into());
    super::tests::install_test_device(&mut runtime, "app", "a", 10);
    super::tests::install_test_device(&mut runtime, "app", "b", 20);
    let devices = runtime.devices.keys().cloned().collect::<Vec<_>>();
    for device in &devices {
        runtime.start_output(&device.control_id()).unwrap();
    }
    let (events, _receiver) = mpsc::channel(8);
    let (relay, task) = spawn_relay_client(events, 8);
    runtime.relay = Some(relay.clone());
    let b = devices[1].clone();
    let key = SourceBindingKey {
        device: b.clone(),
        channel: Channel::A,
    };
    runtime.pending_wave_operations.insert(
        "b-wave".into(),
        PendingWaveOperation {
            device: b.clone(),
            channel: Channel::A,
            generation: runtime.device_source_bindings[&key].generation,
            sent_at: Instant::now(),
        },
    );
    let (_device, _generation, admitted) = runtime
        .prepare_device_stop(&devices[0].control_id())
        .unwrap();
    assert!(admitted.is_some());
    assert!(runtime.output_devices.contains(&b));
    runtime
        .apply_app_message(
            "app",
            &json!({"t":"resp","reqId":"b-wave","error":"B rejected current wave"}),
        )
        .await;
    assert!(!runtime.output_devices.contains(&b));
    assert!(
        runtime
            .snapshot
            .output
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("B rejected current wave"))
    );
    relay.shutdown_now();
    task.await.unwrap();
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
