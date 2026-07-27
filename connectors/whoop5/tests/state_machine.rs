#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use mav_connector_sdk::abi::*;
use mav_connector_sdk::TestDriver;
use mav_connector_whoop5::{Whoop5Connector, CONNECTOR_ID, GEN5_SERVICE};
use whoop_protocol::{crc16_modbus, crc32, decode_frame, Generation};

fn event(sequence: u64, body: EventBody) -> ConnectorEvent {
    ConnectorEvent {
        connector_id: ConnectorId::new(CONNECTOR_ID).unwrap(),
        session_id: SessionId(5),
        sequence: EventSequence(sequence),
        cancellation_generation: CancellationGeneration(0),
        wall_time_ms: Some(1_780_000_000_000),
        body,
    }
}

fn bodies(batch: ActionBatch) -> Vec<ActionBody> {
    batch
        .actions
        .into_iter()
        .map(|action| action.body)
        .collect()
}

fn gen5_frame(payload: &[u8]) -> Vec<u8> {
    let padded = payload.len().div_ceil(4) * 4;
    let declared = u16::try_from(padded + 4).unwrap();
    let length = declared.to_le_bytes();
    let mut frame = vec![0xaa, 1, length[0], length[1], 1, 0, 0, 0];
    let header_crc = crc16_modbus(&frame[..6]).to_le_bytes();
    frame[6..8].copy_from_slice(&header_crc);
    frame.extend_from_slice(payload);
    frame.resize(8 + padded, 0);
    frame.extend_from_slice(&crc32(&frame[8..]).to_le_bytes());
    frame
}

fn drive_to_subscribing(driver: &mut TestDriver<Whoop5Connector>) {
    driver.drive(event(1, EventBody::Activate)).unwrap();
    driver
        .drive(event(
            2,
            EventBody::Advertisement {
                address: "strap-5".to_owned(),
                rssi: -41,
                service_uuids: vec![GEN5_SERVICE.to_owned()],
                manufacturer_data: Vec::new(),
                name: Some("WHOOP MG".to_owned()),
            },
        ))
        .unwrap();
    assert_eq!(
        bodies(
            driver
                .drive(event(3, EventBody::Connected { mtu: 247 }))
                .unwrap()
        ),
        vec![ActionBody::EnsurePaired]
    );
    driver
        .drive(event(
            4,
            EventBody::PairingResult {
                success: true,
                error_code: None,
            },
        ))
        .unwrap();
    driver
        .drive(event(
            5,
            EventBody::ServicesDiscovered {
                service_uuids: vec![GEN5_SERVICE.to_owned(), "180d".to_owned()],
            },
        ))
        .unwrap();
}

#[test]
fn both_scan_identities_require_pairing_before_discovery() {
    for name in ["WHOOP 5.0", "WHOOP MG", "WHOOP"] {
        let mut driver = TestDriver::new(Whoop5Connector::default());
        driver.drive(event(1, EventBody::Activate)).unwrap();
        let advertised = bodies(
            driver
                .drive(event(
                    2,
                    EventBody::Advertisement {
                        address: "strap-5".to_owned(),
                        rssi: -40,
                        service_uuids: vec![GEN5_SERVICE.to_owned()],
                        manufacturer_data: Vec::new(),
                        name: Some(name.to_owned()),
                    },
                ))
                .unwrap(),
        );
        assert!(matches!(
            advertised.as_slice(),
            [ActionBody::StopScan, ActionBody::Connect { .. }]
        ));
        assert_eq!(
            bodies(
                driver
                    .drive(event(3, EventBody::Connected { mtu: 247 }))
                    .unwrap()
            ),
            vec![ActionBody::EnsurePaired]
        );
        assert_eq!(
            bodies(
                driver
                    .drive(event(
                        4,
                        EventBody::PairingResult {
                            success: true,
                            error_code: None,
                        },
                    ))
                    .unwrap()
            ),
            vec![ActionBody::DiscoverServices]
        );
    }
}

#[test]
fn model_identity_accepts_5_and_mg_but_rejects_4() {
    for model in ["5.0", "MG", "WHOOP"] {
        let mut driver = TestDriver::new(Whoop5Connector::default());
        assert!(driver
            .drive(event(
                1,
                EventBody::IdentityRead {
                    field_id: "model-number".to_owned(),
                    bytes: model.as_bytes().to_vec(),
                },
            ))
            .unwrap()
            .actions
            .is_empty());
    }
    let mut driver = TestDriver::new(Whoop5Connector::default());
    let rejected = bodies(
        driver
            .drive(event(
                1,
                EventBody::IdentityRead {
                    field_id: "model-number".to_owned(),
                    bytes: b"4.0".to_vec(),
                },
            ))
            .unwrap(),
    );
    assert!(matches!(rejected[0], ActionBody::EmitDiagnostic { .. }));
    assert_eq!(rejected[1], ActionBody::Disconnect);
}

#[test]
fn confirmed_hello_and_r22_configuration_are_ordered_without_unlock_claim() {
    let mut driver = TestDriver::new(Whoop5Connector::default());
    drive_to_subscribing(&mut driver);
    for (sequence, characteristic) in [
        (6, "standard-heart-rate"),
        (7, "command-response"),
        (8, "events"),
        (9, "data"),
    ] {
        assert!(driver
            .drive(event(
                sequence,
                EventBody::Subscribed {
                    characteristic_id: characteristic.to_owned(),
                },
            ))
            .unwrap()
            .actions
            .is_empty());
    }
    let hello = bodies(
        driver
            .drive(event(
                10,
                EventBody::Subscribed {
                    characteristic_id: "console".to_owned(),
                },
            ))
            .unwrap(),
    );
    let [ActionBody::Write {
        bytes, confirmed, ..
    }] = hello.as_slice()
    else {
        panic!("expected confirmed hello");
    };
    assert!(*confirmed);
    assert_eq!(
        decode_frame(Generation::Gen5, bytes).unwrap(),
        [0x23, 1, 145, 1]
    );

    let query = bodies(
        driver
            .drive(event(
                11,
                EventBody::WriteResult {
                    operation_id: OperationId(10),
                    characteristic_id: "command".to_owned(),
                },
            ))
            .unwrap(),
    );
    let ActionBody::Write { bytes, .. } = &query[0] else {
        panic!("query write");
    };
    assert_eq!(
        decode_frame(Generation::Gen5, bytes).unwrap(),
        [0x23, 2, 3, 1],
        "toggle_realtime_hr must carry the enable byte or live streaming never starts"
    );

    let next = bodies(
        driver
            .drive(event(
                12,
                EventBody::WriteResult {
                    operation_id: OperationId(11),
                    characteristic_id: "command".to_owned(),
                },
            ))
            .unwrap(),
    );
    let ActionBody::Write { bytes, .. } = &next[0] else {
        panic!("next write");
    };
    assert_eq!(decode_frame(Generation::Gen5, bytes).unwrap()[2], 117);

    let flag = bodies(
        driver
            .drive(event(
                13,
                EventBody::WriteResult {
                    operation_id: OperationId(12),
                    characteristic_id: "command".to_owned(),
                },
            ))
            .unwrap(),
    );
    let ActionBody::Write { bytes, .. } = &flag[0] else {
        panic!("flag write");
    };
    let payload = decode_frame(Generation::Gen5, bytes).unwrap();
    assert_eq!(payload[2], 118);

    let first_flag = bodies(
        driver
            .drive(event(
                14,
                EventBody::WriteResult {
                    operation_id: OperationId(13),
                    characteristic_id: "command".to_owned(),
                },
            ))
            .unwrap(),
    );
    let ActionBody::Write { bytes, .. } = &first_flag[0] else {
        panic!("first feature flag write");
    };
    // SET_CONFIG body: 0x01 prefix, 32-byte NUL-padded name, ASCII value, seven zeros.
    let payload = decode_frame(Generation::Gen5, bytes).unwrap();
    assert_eq!(payload[2], 120);
    let mut expected_body = vec![0u8; 41];
    expected_body[0] = 1;
    expected_body[1..19].copy_from_slice(b"enable_r22_packets");
    expected_body[33] = b'2';
    assert_eq!(&payload[3..44], expected_body.as_slice());

    // Sixteen flags in total: fifteen more after the first.
    for sequence in 15..=29 {
        let batch = bodies(
            driver
                .drive(event(
                    sequence,
                    EventBody::WriteResult {
                        operation_id: OperationId(sequence),
                        characteristic_id: "command".to_owned(),
                    },
                ))
                .unwrap(),
        );
        let ActionBody::Write { bytes, .. } = &batch[0] else {
            panic!("remaining feature flag write");
        };
        let payload = decode_frame(Generation::Gen5, bytes).unwrap();
        assert_eq!(payload[2], 120);
        assert_eq!(payload[3], 1, "config prefix");
        assert!(
            matches!(payload[36], b'1' | b'2'),
            "flag value must be an ASCII digit, got {:#04x}",
            payload[36]
        );
    }
    let completed = bodies(
        driver
            .drive(event(
                30,
                EventBody::WriteResult {
                    operation_id: OperationId(30),
                    characteristic_id: "command".to_owned(),
                },
            ))
            .unwrap(),
    );
    assert!(matches!(
        completed[0],
        ActionBody::DeclareCapabilities { .. }
    ));
    assert!(matches!(completed[1], ActionBody::EmitDiagnostic { .. }));
    assert!(matches!(
        completed[2],
        ActionBody::SetTimer {
            token: TimerToken(200),
            ..
        }
    ));
}

#[test]
fn cancellation_disconnect_and_restore_are_generation_safe() {
    let mut driver = TestDriver::new(Whoop5Connector::default());
    drive_to_subscribing(&mut driver);
    let cancelled = bodies(
        driver
            .drive(event(
                6,
                EventBody::Cancel {
                    reason: CancelReason::User,
                },
            ))
            .unwrap(),
    );
    assert_eq!(cancelled.last(), Some(&ActionBody::Disconnect));
    let state = driver.snapshot().unwrap();

    let mut restored = TestDriver::new(Whoop5Connector::default());
    restored
        .drive(event(
            1,
            EventBody::RestoreState {
                bytes: state.clone(),
            },
        ))
        .unwrap();
    assert_eq!(restored.snapshot().unwrap(), state);
    assert!(matches!(
        bodies(restored.drive(event(2, EventBody::Resume)).unwrap()).as_slice(),
        [ActionBody::StartScan { .. }]
    ));
}

#[test]
fn history_idle_response_cursor_ack_and_timeout_are_safe() {
    let streaming_state = vec![0x57, 0x35, 1, 7, 0x1f, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 12, 1];
    let mut driver = TestDriver::new(Whoop5Connector::default());
    driver
        .drive(event(
            1,
            EventBody::RestoreState {
                bytes: streaming_state,
            },
        ))
        .unwrap();
    let range = bodies(
        driver
            .drive(event(
                2,
                EventBody::TimerFired {
                    token: TimerToken(200),
                },
            ))
            .unwrap(),
    );
    let ActionBody::Write { bytes, .. } = &range[0] else {
        panic!("range write");
    };
    assert_eq!(
        decode_frame(Generation::Gen5, bytes).unwrap(),
        [0x23, 1, 34, 0]
    );

    let request = bodies(
        driver
            .drive(event(
                3,
                EventBody::Notification {
                    characteristic_id: "command-response".to_owned(),
                    bytes: gen5_frame(&[0x24, 1, 34, 0, 1]),
                },
            ))
            .unwrap(),
    );
    let ActionBody::Write { bytes, .. } = &request[0] else {
        panic!("history write");
    };
    assert_eq!(
        decode_frame(Generation::Gen5, bytes).unwrap(),
        [0x23, 2, 22, 0]
    );

    let history_end =
        unhex("aa011c00010023d1319102b949596a705d3b000000fdba010010000000000000f269faec");
    let ack = bodies(
        driver
            .drive(event(
                4,
                EventBody::Notification {
                    characteristic_id: "data".to_owned(),
                    bytes: history_end,
                },
            ))
            .unwrap(),
    );
    let [ActionBody::Write { bytes, .. }] = ack.as_slice() else {
        panic!("cursor ack");
    };
    assert_eq!(
        &decode_frame(Generation::Gen5, bytes).unwrap()[..12],
        &[0x23, 3, 23, 1, 0xfd, 0xba, 1, 0, 16, 0, 0, 0]
    );

    let retry = bodies(
        driver
            .drive(event(
                5,
                EventBody::TimerFired {
                    token: TimerToken(201),
                },
            ))
            .unwrap(),
    );
    let ActionBody::Write { bytes, .. } = &retry[0] else {
        panic!("bounded history retry");
    };
    assert_eq!(decode_frame(Generation::Gen5, bytes).unwrap()[2], 22);
}

#[test]
fn long_historical_transfer_extends_the_response_deadline_instead_of_restarting() {
    let streaming_state = vec![0x57, 0x35, 1, 7, 0x1f, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 12, 1];
    let mut driver = TestDriver::new(Whoop5Connector::default());
    driver
        .drive(event(
            1,
            EventBody::RestoreState {
                bytes: streaming_state,
            },
        ))
        .unwrap();
    driver
        .drive(event(
            2,
            EventBody::TimerFired {
                token: TimerToken(200),
            },
        ))
        .unwrap();
    driver
        .drive(event(
            3,
            EventBody::Notification {
                characteristic_id: "command-response".to_owned(),
                bytes: gen5_frame(&[0x24, 1, 34, 0, 1]),
            },
        ))
        .unwrap();

    // MetadataStart marks the transfer as begun; it must re-arm the response deadline rather
    // than leaving the original 5s window from the initial request as the only chance to reply.
    let started = bodies(
        driver
            .drive(event(
                4,
                EventBody::Notification {
                    characteristic_id: "command-response".to_owned(),
                    bytes: gen5_frame(&[0x31, 2, 1]),
                },
            ))
            .unwrap(),
    );
    assert_eq!(
        started,
        vec![ActionBody::SetTimer {
            token: TimerToken(201),
            delay_ms: 5_000,
        }]
    );

    // A record notification arriving mid-transfer must also refresh the deadline: a deep-buffer
    // dump routinely runs past the original 5s window, and without a refresh here the connector
    // would treat that as a dropped response and fire a duplicate SEND_HISTORICAL_DATA (opcode
    // 22) into an in-progress transfer, restarting/duplicating the burst.
    let mut record = vec![0u8; 20];
    record[0] = 47;
    record[1] = 18;
    let emitted = bodies(
        driver
            .drive(event(
                5,
                EventBody::Notification {
                    characteristic_id: "data".to_owned(),
                    bytes: gen5_frame(&record),
                },
            ))
            .unwrap(),
    );
    assert!(matches!(
        emitted.first(),
        Some(ActionBody::SetTimer {
            token: TimerToken(201),
            delay_ms: 5_000,
        })
    ));

    // With the deadline refreshed by real progress, the response timer must not fire a
    // duplicate/restarting retry here in real usage; this only asserts the refresh action
    // shape above, since the discrete-event driver has no wall-clock of its own.

    // Once the transfer completes, the leftover response timer must be cancelled so it cannot
    // spuriously fire a bogus "historical response timed out" diagnostic after we are already
    // back in Streaming.
    let history_end =
        unhex("aa011c00010023d1319102b949596a705d3b000000fdba010010000000000000f269faec");
    driver
        .drive(event(
            6,
            EventBody::Notification {
                characteristic_id: "data".to_owned(),
                bytes: history_end,
            },
        ))
        .unwrap();
    let completed = bodies(
        driver
            .drive(event(
                7,
                EventBody::Notification {
                    characteristic_id: "command-response".to_owned(),
                    bytes: gen5_frame(&[0x31, 3, 3]),
                },
            ))
            .unwrap(),
    );
    assert_eq!(
        completed,
        vec![
            ActionBody::CancelTimer {
                token: TimerToken(201),
            },
            ActionBody::SetTimer {
                token: TimerToken(200),
                delay_ms: 60_000,
            },
        ]
    );
}

/// A v21 deep buffer: 1,240 bytes on the wire, five times the largest ATT payload a phone
/// negotiates.
fn deep_imu_frame() -> Vec<u8> {
    let mut payload = vec![0u8; 1_232];
    payload[0] = 47;
    payload[1] = 21;
    let body = &mut payload[3..];
    body[4..8].copy_from_slice(&1_780_000_000u32.to_le_bytes());
    body[13..15].copy_from_slice(&100u16.to_le_bytes());
    body[619..621].copy_from_slice(&100u16.to_le_bytes());
    gen5_frame(&payload)
}

fn notify(
    driver: &mut TestDriver<Whoop5Connector>,
    sequence: u64,
    bytes: Vec<u8>,
) -> Vec<ActionBody> {
    bodies(
        driver
            .drive(event(
                sequence,
                EventBody::Notification {
                    characteristic_id: "data".to_owned(),
                    bytes,
                },
            ))
            .unwrap(),
    )
}

#[test]
fn deep_imu_notification_splits_at_the_abi_sample_bound() {
    let mut driver = TestDriver::new(Whoop5Connector::default());
    let emitted = notify(&mut driver, 1, deep_imu_frame());
    let [ActionBody::EmitSamples { samples: first, .. }, ActionBody::EmitSamples {
        samples: second, ..
    }] = emitted.as_slice()
    else {
        panic!("deep samples must split into two batches");
    };
    assert_eq!(first.len(), MAX_SAMPLES_PER_ACTION);
    assert_eq!(second.len(), 600 - MAX_SAMPLES_PER_ACTION);
}

/// BLE fragment boundaries are not frame boundaries. A frame delivered twenty bytes at a time
/// must yield exactly the samples the whole frame yields, and nothing before it is complete.
#[test]
fn a_frame_delivered_in_fragments_matches_the_whole_frame() {
    let frame = deep_imu_frame();
    let mut whole = TestDriver::new(Whoop5Connector::default());
    let expected = notify(&mut whole, 1, frame.clone());

    let mut fragmented = TestDriver::new(Whoop5Connector::default());
    let mut emitted = Vec::new();
    let last = frame.len().div_ceil(20);
    for (index, chunk) in frame.chunks(20).enumerate() {
        let actions = notify(&mut fragmented, index as u64 + 1, chunk.to_vec());
        if index + 1 < last {
            assert!(actions.is_empty(), "fragment {index} emitted early");
        }
        emitted.extend(actions);
    }
    assert_eq!(emitted, expected);
}

/// Two frames packed into a single notification both decode; neither is lost to the other.
#[test]
fn frames_packed_into_one_notification_both_decode() {
    let frame = deep_imu_frame();
    let mut packed = frame.clone();
    packed.extend_from_slice(&frame);

    let mut driver = TestDriver::new(Whoop5Connector::default());
    let emitted = notify(&mut driver, 1, packed);
    let counts = emitted
        .iter()
        .map(|body| match body {
            ActionBody::EmitSamples { samples, .. } => samples.len(),
            other => panic!("unexpected action {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        counts,
        vec![
            MAX_SAMPLES_PER_ACTION,
            600 - MAX_SAMPLES_PER_ACTION,
            MAX_SAMPLES_PER_ACTION,
            600 - MAX_SAMPLES_PER_ACTION,
        ]
    );
}

/// A partial frame must not survive a reconnect: the tail of the old frame would splice onto
/// the head of a new one.
#[test]
fn a_partial_frame_is_dropped_on_reconnect() {
    let frame = deep_imu_frame();
    let mut driver = TestDriver::new(Whoop5Connector::default());
    assert!(notify(&mut driver, 1, frame[..600].to_vec()).is_empty());
    driver.drive(event(2, EventBody::Resume)).unwrap();
    // The stale tail alone decodes to nothing; the buffer was cleared.
    assert!(notify(&mut driver, 3, frame[600..].to_vec()).is_empty());
    assert_eq!(notify(&mut driver, 4, frame).len(), 2);
}

fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn packaged_fixtures_match_native_actions_and_state() {
    let metadata = mav_connector_whoop5::metadata().unwrap();
    for fixture in metadata.fixtures.cases {
        let mut driver = TestDriver::new(Whoop5Connector::default());
        let mut next = 0;
        if fixture.initial_state.is_empty() {
            assert_eq!(
                driver.init(fixture.events[0].clone()).unwrap(),
                fixture.expected[0]
            );
            next = 1;
        } else {
            let mut restore = fixture.events[0].clone();
            restore.body = EventBody::RestoreState {
                bytes: fixture.initial_state,
            };
            assert!(driver.init(restore).unwrap().actions.is_empty());
        }
        for index in next..fixture.events.len() {
            assert_eq!(
                driver.drive(fixture.events[index].clone()).unwrap(),
                fixture.expected[index],
                "fixture {} action {index}",
                fixture.name
            );
        }
        assert_eq!(
            driver.snapshot_hash().unwrap(),
            fixture.expected_state_hash,
            "fixture {} state",
            fixture.name
        );
    }
}

#[test]
fn packaged_parity_covers_history_restart_and_malformed_input() {
    let names = mav_connector_whoop5::metadata()
        .unwrap()
        .fixtures
        .cases
        .into_iter()
        .map(|fixture| fixture.name)
        .collect::<Vec<_>>();
    for required in [
        "history-cursor-retry",
        "state-restart",
        "malformed-frame",
        "frame-split-across-notifications",
        "frames-packed-in-one-notification",
    ] {
        assert!(
            names.iter().any(|name| name == required),
            "missing {required}"
        );
    }
}

#[test]
fn live_streaming_defers_historical_offload_instead_of_preempting_it() {
    let streaming_state = vec![0x57, 0x35, 1, 7, 0x1f, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 12, 1];
    let mut driver = TestDriver::new(Whoop5Connector::default());
    driver
        .drive(event(
            1,
            EventBody::RestoreState {
                bytes: streaming_state,
            },
        ))
        .unwrap();

    let mut live = event(
        2,
        EventBody::Notification {
            characteristic_id: "standard-heart-rate".to_owned(),
            bytes: vec![0x10, 64, 0x33, 0x03],
        },
    );
    live.wall_time_ms = Some(1_780_000_000_000);
    assert!(matches!(
        bodies(driver.drive(live).unwrap()).as_slice(),
        [ActionBody::EmitSamples { .. }]
    ));

    // Idle deadline lands one second after a live sample: offload must stand down and re-arm.
    let mut fired = event(
        3,
        EventBody::TimerFired {
            token: TimerToken(200),
        },
    );
    fired.wall_time_ms = Some(1_780_000_001_000);
    assert_eq!(
        bodies(driver.drive(fired).unwrap()),
        vec![ActionBody::SetTimer {
            token: TimerToken(200),
            delay_ms: 60_000,
        }],
        "live streaming must defer offload, not issue GET_DATA_RANGE"
    );

    // After a real gap the offload proceeds exactly as before.
    let mut stale = event(
        4,
        EventBody::TimerFired {
            token: TimerToken(200),
        },
    );
    stale.wall_time_ms = Some(1_780_000_060_000);
    let range = bodies(driver.drive(stale).unwrap());
    let ActionBody::Write { bytes, .. } = &range[0] else {
        panic!("expected GET_DATA_RANGE after live data stops");
    };
    assert_eq!(
        decode_frame(Generation::Gen5, bytes).unwrap(),
        [0x23, 1, 34, 0]
    );
}

/// The console channel carries firmware log text, not measurements. Routing it through the record
/// decoder would invent samples out of ASCII.
#[test]
fn console_frames_become_diagnostics_and_never_samples() {
    // A real console frame carries packet type 50 at [0] — the live capture starts 0x32 — and the
    // text sits behind a ten-byte record header. Routing is by that type, not by which
    // characteristic delivered it: a live strap sends console output on the data channel too.
    let mut payload = vec![0u8; 10];
    payload[0] = 50;
    payload.extend_from_slice(b"RTC timestamp invalid; not saving\n");
    let mut driver = TestDriver::new(Whoop5Connector::default());
    let emitted = bodies(
        driver
            .drive(event(
                1,
                EventBody::Notification {
                    characteristic_id: "console".to_owned(),
                    bytes: gen5_frame(&payload),
                },
            ))
            .unwrap(),
    );
    let [ActionBody::EmitDiagnostic {
        level,
        code,
        message,
    }] = emitted.as_slice()
    else {
        panic!("console must produce exactly one diagnostic: {emitted:?}");
    };
    assert_eq!(*level, DiagnosticLevel::Info);
    assert_eq!(code, "whoop5-console");
    assert_eq!(message, "RTC timestamp invalid; not saving\n");
}
