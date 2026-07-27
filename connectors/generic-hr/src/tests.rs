//! What the connector promises: it finds a standard heart-rate device, publishes its beats on the
//! stream the device's own sensor location justifies, times every interval individually, and stays
//! off the radio otherwise.

use super::*;
use mav_connector_sdk::TestDriver;

const ARRIVED_MS: i64 = 1_780_000_000_000;

fn event(sequence: u64, body: EventBody) -> ConnectorEvent {
    ConnectorEvent {
        connector_id: ConnectorId::new(CONNECTOR_ID).expect("a valid connector id"),
        session_id: SessionId(1),
        sequence: EventSequence(sequence),
        cancellation_generation: CancellationGeneration(0),
        wall_time_ms: Some(ARRIVED_MS),
        body,
    }
}

fn bodies(batch: &ActionBatch) -> Vec<&ActionBody> {
    batch.actions.iter().map(|action| &action.body).collect()
}

fn samples(batch: &ActionBatch) -> Vec<WireSample> {
    batch
        .actions
        .iter()
        .flat_map(|action| match &action.body {
            ActionBody::EmitSamples { samples, .. } => samples.clone(),
            _ => Vec::new(),
        })
        .collect()
}

/// Drive a device through connect and discovery, reading its sensor location as `site`.
fn connected(site: u8) -> TestDriver<GenericHeartRateConnector> {
    let mut driver = TestDriver::new(GenericHeartRateConnector::default());
    for body in [
        EventBody::Activate,
        EventBody::Advertisement {
            address: "AA:BB:CC:DD:EE:FF".to_owned(),
            rssi: -55,
            service_uuids: vec![ble_sig::HEART_RATE_SERVICE.to_owned()],
            manufacturer_data: Vec::new(),
            name: Some("Strap".to_owned()),
        },
        EventBody::Connected { mtu: 23 },
        EventBody::ServicesDiscovered {
            service_uuids: vec![
                ble_sig::HEART_RATE_SERVICE.to_owned(),
                ble_sig::BATTERY_SERVICE.to_owned(),
            ],
        },
        EventBody::ReadResult {
            operation_id: OperationId(4),
            characteristic_id: SITE_ID.to_owned(),
            bytes: vec![site],
        },
        EventBody::Subscribed {
            characteristic_id: MEASUREMENT_ID.to_owned(),
        },
    ] {
        driver
            .drive(event(1, body))
            .expect("the happy path produces valid actions");
    }
    driver
}

#[test]
fn activation_scans_for_the_standard_heart_rate_service() {
    let mut driver = TestDriver::new(GenericHeartRateConnector::default());
    let batch = driver
        .drive(event(1, EventBody::Activate))
        .expect("activate");
    assert_eq!(
        bodies(&batch),
        vec![&ActionBody::StartScan {
            service_uuids: vec![ble_sig::HEART_RATE_SERVICE.to_owned()],
            manufacturer_ids: Vec::new(),
        }]
    );
}

/// The claim this connector exists to get right. The same bytes from a chest strap and from a
/// wrist sensor are the same numbers about two different physiological events, and only the first
/// may reach the wearer as heart-rate variability.
#[test]
fn the_sensor_location_decides_which_interval_stream_the_beats_reach() {
    // flags 0x16: contact supported and detected, intervals present; 819/1024 s = 800 ms.
    let measurement = vec![0x16, 62, 0x33, 0x03];
    for (site, expected) in [
        (1u8, "rr-interval"),
        (2, "pulse-interval"),
        (0, "pulse-interval"),
    ] {
        let mut driver = connected(site);
        let batch = driver
            .drive(event(
                7,
                EventBody::Notification {
                    characteristic_id: MEASUREMENT_ID.to_owned(),
                    bytes: measurement.clone(),
                },
            ))
            .expect("measurement");
        let intervals: Vec<WireSample> = samples(&batch)
            .into_iter()
            .filter(|sample| sample.unit == "milliseconds")
            .collect();
        assert_eq!(intervals.len(), 1, "site {site}");
        assert_eq!(intervals[0].stream, expected, "site {site}");
        assert_eq!(intervals[0].value_microunits, 800_000_000);
    }
}

/// A device that never says where it is gets the optical answer, because the error that matters is
/// calling an optical pulse an electrocardiogram.
#[test]
fn an_unread_sensor_location_defaults_to_the_optical_claim() {
    let mut driver = TestDriver::new(GenericHeartRateConnector::default());
    driver
        .drive(event(1, EventBody::Activate))
        .expect("activate");
    assert_eq!(
        driver.into_inner().interval_stream(),
        "pulse-interval",
        "no location read, so no HRV claim"
    );
}

/// Four intervals in one packet are four beats at four different instants. Stamping them all with
/// the arrival time is what makes a variability series read a burst boundary as a heartbeat.
#[test]
fn every_interval_in_a_packet_gets_its_own_instant() {
    let mut driver = connected(1);
    let batch = driver
        .drive(event(
            7,
            EventBody::Notification {
                characteristic_id: MEASUREMENT_ID.to_owned(),
                // flags 0x10, rate 60, then 800 ms, 850 ms, 900 ms.
                bytes: vec![0x10, 60, 0x33, 0x03, 0x66, 0x03, 0x9A, 0x03],
            },
        ))
        .expect("measurement");

    let intervals: Vec<(i64, i64)> = samples(&batch)
        .into_iter()
        .filter(|sample| sample.unit == "milliseconds")
        .map(|sample| {
            (
                sample.device_time_ms.expect("timed"),
                sample.value_microunits / 1_000_000,
            )
        })
        .collect();
    assert_eq!(
        intervals,
        vec![
            (ARRIVED_MS - 1_750, 800),
            (ARRIVED_MS - 900, 850),
            (ARRIVED_MS, 900),
        ]
    );
}

#[test]
fn heart_rate_and_contact_are_published_alongside_the_beats() {
    let mut driver = connected(1);
    let batch = driver
        .drive(event(
            7,
            EventBody::Notification {
                characteristic_id: MEASUREMENT_ID.to_owned(),
                bytes: vec![0x16, 62, 0x33, 0x03],
            },
        ))
        .expect("measurement");
    let published: Vec<(String, i64)> = samples(&batch)
        .into_iter()
        .map(|sample| (sample.stream, sample.value_microunits / 1_000_000))
        .collect();
    assert!(published.contains(&("heart-rate".to_owned(), 62)));
    assert!(published.contains(&("skin-contact".to_owned(), 1)));
}

#[test]
fn a_battery_notification_becomes_a_battery_sample() {
    let mut driver = connected(1);
    let batch = driver
        .drive(event(
            8,
            EventBody::Notification {
                characteristic_id: BATTERY_ID.to_owned(),
                bytes: vec![81],
            },
        ))
        .expect("battery");
    assert_eq!(
        samples(&batch)
            .into_iter()
            .map(|sample| (sample.stream, sample.value_microunits))
            .collect::<Vec<_>>(),
        vec![("battery-soc".to_owned(), 81_000_000)]
    );
}

/// Nothing here polls. The only timer is the silence watchdog, and every measurement pushes it out
/// rather than adding another — a connector that sets a fresh timer per beat wakes the phone once
/// a second for no reason.
#[test]
fn a_measurement_refreshes_the_watchdog_and_asks_for_nothing_else() {
    let mut driver = connected(1);
    let batch = driver
        .drive(event(
            7,
            EventBody::Notification {
                characteristic_id: MEASUREMENT_ID.to_owned(),
                bytes: vec![0x10, 60, 0x33, 0x03],
            },
        ))
        .expect("measurement");
    let requested: Vec<&ActionBody> = bodies(&batch);
    assert!(requested.iter().any(|body| matches!(
        body,
        ActionBody::SetTimer {
            token: SILENCE_TIMER,
            ..
        }
    )));
    assert!(
        requested.iter().all(|body| matches!(
            body,
            ActionBody::SetTimer { .. } | ActionBody::EmitSamples { .. }
        )),
        "a beat must not trigger reads, writes or scans"
    );
}

#[test]
fn a_minute_of_silence_drops_the_link_rather_than_probing_it() {
    let mut driver = connected(1);
    let batch = driver
        .drive(event(
            9,
            EventBody::TimerFired {
                token: SILENCE_TIMER,
            },
        ))
        .expect("timeout");
    assert!(bodies(&batch).contains(&&ActionBody::Disconnect));
}

#[test]
fn a_malformed_measurement_is_an_error_rather_than_a_guessed_heart_rate() {
    let mut driver = connected(1);
    let failure = driver.drive(event(
        7,
        EventBody::Notification {
            characteristic_id: MEASUREMENT_ID.to_owned(),
            bytes: vec![0x01],
        },
    ));
    assert!(matches!(failure, Err(ConnectorError::InvalidWire(_))));
}

#[test]
fn declared_streams_name_the_one_interval_stream_this_device_actually_produces() {
    assert!(connected(1)
        .into_inner()
        .streams()
        .contains(&"rr-interval".to_owned()));
    let optical = connected(2).into_inner().streams();
    assert!(optical.contains(&"pulse-interval".to_owned()));
    assert!(!optical.contains(&"rr-interval".to_owned()));
}

#[test]
fn the_packaged_metadata_is_internally_consistent() {
    let metadata = metadata().expect("metadata builds");
    assert_eq!(metadata.manifest.display_name, "Generic HR Monitor");
    assert_eq!(metadata.manifest.connector_id.as_str(), CONNECTOR_ID);
    assert!(!metadata.fixtures.cases.is_empty());
}

/// The host rejects a zero operation id, so a connector that counts from zero cannot get its first
/// action past the boundary — every session would die on activation.
#[test]
fn the_first_action_carries_a_positive_operation_id() {
    let mut driver = TestDriver::new(GenericHeartRateConnector::default());
    let batch = driver
        .drive(event(1, EventBody::Activate))
        .expect("activate");
    let first = batch.actions.first().expect("activation acts");
    assert!(first.operation_id.0 > 0);
    assert!(first.deadline_token.0 > 0);
}
