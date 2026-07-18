#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use mav_connector_sdk::abi::*;
use mav_connector_sdk::TestDriver;
use mav_connector_whoop4::{Whoop4Connector, CONNECTOR_ID, GEN4_SERVICE};
use sha2::{Digest, Sha256};
use whoop_protocol::{crc32, crc8, decode_frame, Generation};

fn event(sequence: u64, body: EventBody) -> ConnectorEvent {
    ConnectorEvent {
        connector_id: ConnectorId::new(CONNECTOR_ID).unwrap(),
        session_id: SessionId(4),
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

fn drive_to_subscribing(driver: &mut TestDriver<Whoop4Connector>) {
    driver.drive(event(1, EventBody::Activate)).unwrap();
    driver
        .drive(event(
            2,
            EventBody::Advertisement {
                address: "strap-4".to_owned(),
                rssi: -42,
                service_uuids: vec![GEN4_SERVICE.to_owned()],
                manufacturer_data: Vec::new(),
                name: Some("WHOOP 4.0".to_owned()),
            },
        ))
        .unwrap();
    driver
        .drive(event(3, EventBody::Connected { mtu: 247 }))
        .unwrap();
    driver
        .drive(event(
            4,
            EventBody::ServicesDiscovered {
                service_uuids: vec![GEN4_SERVICE.to_owned(), "180d".to_owned()],
            },
        ))
        .unwrap();
}

fn drive_to_streaming(driver: &mut TestDriver<Whoop4Connector>) {
    drive_to_subscribing(driver);
    for (sequence, characteristic) in [
        (5, "standard-heart-rate"),
        (6, "command-response"),
        (7, "events"),
        (8, "data"),
    ] {
        driver
            .drive(event(
                sequence,
                EventBody::Subscribed {
                    characteristic_id: characteristic.to_owned(),
                },
            ))
            .unwrap();
    }
    driver
        .drive(event(
            9,
            EventBody::WriteResult {
                operation_id: OperationId(9),
                characteristic_id: "command".to_owned(),
            },
        ))
        .unwrap();
}

fn gen4_frame(payload: &[u8]) -> Vec<u8> {
    let declared = u16::try_from(payload.len() + 4).unwrap();
    let mut frame = vec![
        0xaa,
        declared.to_le_bytes()[0],
        declared.to_le_bytes()[1],
        0,
    ];
    frame[3] = crc8(&frame[1..3]);
    frame.extend_from_slice(payload);
    frame.extend_from_slice(&crc32(payload).to_le_bytes());
    frame
}

#[test]
fn advertisement_identity_and_unbonded_connection_are_exact() {
    let mut driver = TestDriver::new(Whoop4Connector::default());
    assert_eq!(
        bodies(driver.drive(event(1, EventBody::Activate)).unwrap()),
        vec![ActionBody::StartScan {
            service_uuids: vec![GEN4_SERVICE.to_owned(), "180d".to_owned()],
            manufacturer_ids: Vec::new(),
        }]
    );
    assert!(driver
        .drive(event(
            2,
            EventBody::Advertisement {
                address: "wrong".to_owned(),
                rssi: -30,
                service_uuids: vec!["fd4b0001-cce1-4033-93ce-002d5875f58a".to_owned()],
                manufacturer_data: Vec::new(),
                name: Some("WHOOP 5.0".to_owned()),
            },
        ))
        .unwrap()
        .actions
        .is_empty());
    assert_eq!(
        bodies(
            driver
                .drive(event(
                    3,
                    EventBody::Advertisement {
                        address: "strap-4".to_owned(),
                        rssi: -42,
                        service_uuids: vec![GEN4_SERVICE.to_owned()],
                        manufacturer_data: Vec::new(),
                        name: Some("WHOOP 4.0".to_owned()),
                    },
                ))
                .unwrap()
        ),
        vec![
            ActionBody::StopScan,
            ActionBody::Connect {
                address: "strap-4".to_owned(),
            },
        ]
    );
    let connected = bodies(
        driver
            .drive(event(4, EventBody::Connected { mtu: 247 }))
            .unwrap(),
    );
    assert_eq!(connected, vec![ActionBody::DiscoverServices]);
    assert!(!connected.contains(&ActionBody::EnsurePaired));
}

#[test]
fn subscriptions_then_gen4_hello_are_ordered() {
    let mut driver = TestDriver::new(Whoop4Connector::default());
    drive_to_subscribing(&mut driver);
    for (sequence, characteristic) in [
        (5, "standard-heart-rate"),
        (6, "command-response"),
        (7, "events"),
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
                8,
                EventBody::Subscribed {
                    characteristic_id: "data".to_owned(),
                },
            ))
            .unwrap(),
    );
    let [ActionBody::Write {
        characteristic_id,
        bytes,
        confirmed,
    }] = hello.as_slice()
    else {
        panic!("expected one hello write: {hello:?}");
    };
    assert_eq!(characteristic_id, "command");
    assert!(*confirmed);
    assert_eq!(
        whoop_protocol::decode_frame(whoop_protocol::Generation::Gen4, bytes).unwrap(),
        [0x23, 1, 35]
    );
}

#[test]
fn disconnect_snapshot_restores_without_restarting_the_old_session() {
    let mut driver = TestDriver::new(Whoop4Connector::default());
    drive_to_subscribing(&mut driver);
    let before = driver.snapshot().unwrap();
    driver
        .drive(event(5, EventBody::Disconnected { reason_code: 19 }))
        .unwrap();
    let disconnected = driver.snapshot().unwrap();
    assert_ne!(before, disconnected);

    let mut restored = TestDriver::new(Whoop4Connector::default());
    assert!(restored
        .drive(event(
            1,
            EventBody::RestoreState {
                bytes: disconnected.clone(),
            },
        ))
        .unwrap()
        .actions
        .is_empty());
    assert_eq!(restored.snapshot().unwrap(), disconnected);
    assert_eq!(
        bodies(restored.drive(event(2, EventBody::Resume)).unwrap()),
        vec![ActionBody::StartScan {
            service_uuids: vec![GEN4_SERVICE.to_owned(), "180d".to_owned()],
            manufacturer_ids: Vec::new(),
        }]
    );
}

#[test]
fn model_identity_refuses_a_gen5_device() {
    let mut driver = TestDriver::new(Whoop4Connector::default());
    let rejected = bodies(
        driver
            .drive(event(
                1,
                EventBody::IdentityRead {
                    field_id: "model-number".to_owned(),
                    bytes: b"MG".to_vec(),
                },
            ))
            .unwrap(),
    );
    assert!(matches!(rejected[0], ActionBody::EmitDiagnostic { .. }));
    assert_eq!(rejected[1], ActionBody::Disconnect);
}

#[test]
fn history_requests_retry_and_ack_cursor_without_force_trim() {
    let mut driver = TestDriver::new(Whoop4Connector::default());
    drive_to_streaming(&mut driver);

    let range = bodies(
        driver
            .drive(event(
                10,
                EventBody::TimerFired {
                    token: TimerToken(100),
                },
            ))
            .unwrap(),
    );
    let ActionBody::Write { bytes, .. } = &range[0] else {
        panic!("range must start with a write");
    };
    assert_eq!(
        decode_frame(Generation::Gen4, bytes).unwrap(),
        [0x23, 2, 34]
    );
    assert!(matches!(
        range[1],
        ActionBody::SetTimer {
            token: TimerToken(101),
            ..
        }
    ));

    let response = gen4_frame(&[0x24, 2, 34, 1]);
    let request = bodies(
        driver
            .drive(event(
                11,
                EventBody::Notification {
                    characteristic_id: "command-response".to_owned(),
                    bytes: response,
                },
            ))
            .unwrap(),
    );
    let ActionBody::Write { bytes, .. } = &request[0] else {
        panic!("range response must request history");
    };
    assert_eq!(
        decode_frame(Generation::Gen4, bytes).unwrap(),
        [0x23, 3, 22]
    );

    let record =
        include_str!("../../../crates/whoop-protocol/tests/fixtures/whoop_rs_gen4_v24.hex");
    let emitted = bodies(
        driver
            .drive(event(
                12,
                EventBody::Notification {
                    characteristic_id: "data".to_owned(),
                    bytes: unhex(record.trim()),
                },
            ))
            .unwrap(),
    );
    assert!(matches!(
        emitted.as_slice(),
        [ActionBody::EmitSamples { .. }]
    ));

    let cursor = [1, 2, 3, 4, 5, 6, 7, 8];
    let mut metadata = vec![0x31, 9, 2];
    metadata.extend_from_slice(&[0; 10]);
    metadata.extend_from_slice(&cursor);
    let ack = bodies(
        driver
            .drive(event(
                13,
                EventBody::Notification {
                    characteristic_id: "data".to_owned(),
                    bytes: gen4_frame(&metadata),
                },
            ))
            .unwrap(),
    );
    let [ActionBody::Write { bytes, .. }] = ack.as_slice() else {
        panic!("metadata end must produce one ack");
    };
    assert_eq!(
        decode_frame(Generation::Gen4, bytes).unwrap(),
        [0x23, 4, 23, 1, 2, 3, 4, 5, 6, 7, 8]
    );

    let timeout = bodies(
        driver
            .drive(event(
                14,
                EventBody::TimerFired {
                    token: TimerToken(101),
                },
            ))
            .unwrap(),
    );
    let ActionBody::Write { bytes, .. } = &timeout[0] else {
        panic!("first timeout retries safely");
    };
    assert_eq!(decode_frame(Generation::Gen4, bytes).unwrap()[2], 22);
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
    let metadata = mav_connector_whoop4::metadata().unwrap();
    for fixture in metadata.fixtures.cases {
        let mut driver = TestDriver::new(Whoop4Connector::default());
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
            <[u8; 32]>::from(Sha256::digest(driver.snapshot().unwrap())),
            fixture.expected_state_hash,
            "fixture {} state",
            fixture.name
        );
    }
}

#[test]
fn packaged_parity_covers_history_restart_and_malformed_input() {
    let names = mav_connector_whoop4::metadata()
        .unwrap()
        .fixtures
        .cases
        .into_iter()
        .map(|fixture| fixture.name)
        .collect::<Vec<_>>();
    for required in ["history-cursor-retry", "state-restart", "malformed-frame"] {
        assert!(
            names.iter().any(|name| name == required),
            "missing {required}"
        );
    }
}
