#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use mav_connector_whoop5::decode::decode_payload;
use whoop_protocol::{decode_frame, Generation};

fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn real_v18_metrics_and_v26_ppg_replay_exactly() {
    let v18 = include_str!("../../../crates/whoop-protocol/tests/fixtures/whoop_rs_gen5_v18.hex");
    let payload = decode_frame(Generation::Gen5, &unhex(v18.trim())).unwrap();
    let samples = decode_payload(&payload).unwrap();
    assert_eq!(samples.len(), 11);
    assert_eq!(samples[0].stream, "heart-rate");
    assert_eq!(samples[0].value_microunits, 102_000_000);
    assert_eq!(samples[1].value_microunits, 602_000_000);
    assert_eq!(samples[2].value_microunits, 613_000_000);
    assert_eq!(samples[6].stream, "skin-temp");
    assert_eq!(samples[6].value_microunits, 30_570_000);
    assert_eq!(samples[7].stream, "step-count");
    assert_eq!(samples[10].stream, "signal-quality");

    let v26 = include_str!("../../../crates/whoop-protocol/tests/fixtures/whoop_rs_gen5_v26.hex");
    let payload = decode_frame(Generation::Gen5, &unhex(v26.trim())).unwrap();
    let samples = decode_payload(&payload).unwrap();
    assert_eq!(samples.len(), 24);
    assert!(samples.iter().all(|sample| sample.stream == "ppg"));
    assert!(samples.iter().any(|sample| sample.value_microunits < 0));
}

#[test]
fn synthetic_deep_buffers_are_bounded_and_structurally_gated() {
    let mut v21 = vec![0u8; 1_232];
    v21[0] = 47;
    v21[1] = 21;
    {
        let body = &mut v21[3..];
        body[4..8].copy_from_slice(&1_780_000_000u32.to_le_bytes());
        body[13..15].copy_from_slice(&100u16.to_le_bytes());
        body[619..621].copy_from_slice(&100u16.to_le_bytes());
        body[17..19].copy_from_slice(&4096i16.to_le_bytes());
        body[629..631].copy_from_slice(&250i16.to_le_bytes());
    }
    let samples = decode_payload(&v21).unwrap();
    assert_eq!(samples.len(), 600);
    assert_eq!(samples[0].stream, "imu");
    assert_eq!(samples[0].value_microunits, 4_096_000_000);
    assert_eq!(samples[300].stream, "gyro");
    assert_eq!(samples[300].value_microunits, 250_000_000);

    v21[16..18].copy_from_slice(&99u16.to_le_bytes());
    assert!(decode_payload(&v21).unwrap().is_empty());

    let mut v20 = vec![0u8; 2_027];
    v20[0] = 47;
    v20[1] = 20;
    let body = &mut v20[3..];
    body[4..8].copy_from_slice(&1_780_000_000u32.to_le_bytes());
    body[17..19].copy_from_slice(&1400u16.to_le_bytes());
    body[20..22].copy_from_slice(&2800u16.to_le_bytes());
    body[36..40].copy_from_slice(&12345u32.to_le_bytes());
    body[40..44].copy_from_slice(&0x000f_fffbu32.to_le_bytes());
    let samples = decode_payload(&v20).unwrap();
    assert_eq!(samples.len(), 150);
    assert_eq!(samples[0].value_microunits, 12_345_000_000);
    assert_eq!(samples[1].value_microunits, -5_000_000);
}

#[test]
fn realtime_events_and_malformed_records_are_explicit() {
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
    assert_eq!(decode_payload(&battery).unwrap()[0].stream, "battery-soc");
    assert!(decode_payload(&[47, 27, 0x80, 0]).is_err());
    assert!(decode_payload(&[48, 0, 3]).is_err());
}
