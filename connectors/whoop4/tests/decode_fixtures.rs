#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use mav_connector_sdk::abi::WireSample;
use mav_connector_whoop4::decode::decode_payload;
use whoop_protocol::{decode_frame, Generation};

fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn sample(
    stream: &str,
    value_microunits: i64,
    time_ms: i64,
    sequence: u32,
    unit: &str,
) -> WireSample {
    WireSample {
        stream: stream.to_owned(),
        value_microunits,
        device_time_ms: Some(time_ms),
        sequence,
        unit: unit.to_owned(),
    }
}

#[test]
fn real_v24_record_replays_all_admitted_values() {
    let wire = include_str!("../../../crates/whoop-protocol/tests/fixtures/whoop_rs_gen4_v24.hex");
    let payload = decode_frame(Generation::Gen4, &unhex(wire.trim())).unwrap();
    let samples = decode_payload(&payload).unwrap();
    assert_eq!(samples.len(), 10);
    assert_eq!(
        samples[0],
        sample(
            "heart-rate",
            109_000_000,
            1_780_928_574_000,
            0,
            "beats-per-minute"
        )
    );
    assert_eq!(
        samples[1],
        sample(
            "rr-interval",
            555_000_000,
            1_780_928_574_000,
            0,
            "milliseconds"
        )
    );
    assert_eq!(
        samples[2],
        sample(
            "rr-interval",
            564_000_000,
            1_780_928_574_000,
            1,
            "milliseconds"
        )
    );
    assert_eq!(
        samples[6],
        sample("spo2-raw", 592_000_000, 1_780_928_574_000, 0, "counts")
    );
    assert_eq!(
        samples[7],
        sample("spo2-raw", 612_000_000, 1_780_928_574_000, 1, "counts")
    );
    assert_eq!(
        samples[8],
        sample(
            "skin-temp",
            861_000_000,
            1_780_928_574_000,
            0,
            "degrees-celsius"
        )
    );
    assert_eq!(
        samples[9],
        sample("resp-raw", 3_073_000_000, 1_780_928_574_000, 0, "counts")
    );

    let mut version_twelve = payload;
    version_twelve[1] = 12;
    assert_eq!(decode_payload(&version_twelve).unwrap(), samples);
}

#[test]
fn real_v25_and_generic_versions_are_generation_local() {
    let wire = include_str!("../../../crates/whoop-protocol/tests/fixtures/whoop_rs_gen4_v25.hex");
    let payload = decode_frame(Generation::Gen4, &unhex(wire.trim())).unwrap();
    let samples = decode_payload(&payload).unwrap();
    assert_eq!(samples.len(), 3);
    assert!(samples.iter().all(|sample| sample.stream == "gravity"));

    for version in [5, 7, 9] {
        let mut record = vec![0x2f, version, 0x80];
        let mut body = vec![0u8; 20];
        body[4..8].copy_from_slice(&1_780_000_000u32.to_le_bytes());
        body[14] = 63;
        body[15] = 2;
        body[16..18].copy_from_slice(&800u16.to_le_bytes());
        body[18..20].copy_from_slice(&810u16.to_le_bytes());
        record.extend(body);
        assert_eq!(
            decode_payload(&record).unwrap().len(),
            3,
            "version {version}"
        );
    }
}

#[test]
fn realtime_and_event_packets_decode_without_analytics() {
    let mut realtime = vec![0u8; 14];
    realtime[0] = 40;
    realtime[2..6].copy_from_slice(&1_780_000_000u32.to_le_bytes());
    realtime[8] = 64;
    realtime[9] = 2;
    realtime[10..12].copy_from_slice(&800u16.to_le_bytes());
    realtime[12..14].copy_from_slice(&810u16.to_le_bytes());
    assert_eq!(decode_payload(&realtime).unwrap().len(), 3);

    let mut battery = vec![0u8; 24];
    battery[0] = 48;
    battery[2] = 3;
    battery[4..8].copy_from_slice(&1_780_000_000u32.to_le_bytes());
    battery[13..15].copy_from_slice(&812u16.to_le_bytes());
    assert_eq!(
        decode_payload(&battery).unwrap(),
        vec![sample(
            "battery-soc",
            81_200_000,
            1_780_000_000_000,
            0,
            "percent"
        )]
    );

    let mut wrist = battery;
    wrist[2] = 9;
    assert_eq!(
        decode_payload(&wrist).unwrap(),
        vec![sample(
            "wrist-state",
            1_000_000,
            1_780_000_000_000,
            0,
            "boolean"
        )]
    );
}

#[test]
fn malformed_and_unmapped_records_fail_closed() {
    assert!(decode_payload(&[47, 24, 0x80, 0]).is_err());
    assert!(decode_payload(&[47, 27, 0x80, 0, 0, 0]).is_err());
    assert!(decode_payload(&[48, 0, 3]).is_err());
}
