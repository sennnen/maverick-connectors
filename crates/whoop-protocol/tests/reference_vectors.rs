#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use whoop_protocol::{
    build_command, classify_record, crc16_modbus, crc32, crc8, decode_control, decode_frame,
    get_data_range, history_ack, request_history, Control, ControlResult, Generation,
    ProtocolError, RecordDecoder,
};

fn unhex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = core::str::from_utf8(pair).unwrap();
            u8::from_str_radix(text, 16).unwrap()
        })
        .collect()
}

#[test]
fn standard_crc_checks_match() {
    assert_eq!(crc8(b"123456789"), 0xf4);
    assert_eq!(crc16_modbus(b"123456789"), 0x4b37);
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
}

#[test]
fn maverick_generation_goldens_decode_exactly() {
    let gen4 = unhex("aa0800a823070a015511641b");
    let gen5 = unhex("aa0108000001e67123019101363e5c8d");

    assert_eq!(
        decode_frame(Generation::Gen4, &gen4).unwrap(),
        [0x23, 0x07, 0x0a, 0x01]
    );
    assert_eq!(
        decode_frame(Generation::Gen5, &gen5).unwrap(),
        [0x23, 0x01, 0x91, 0x01]
    );
    assert_eq!(
        build_command(Generation::Gen5, 1, 0x91, &[1]).unwrap(),
        gen5
    );
}

/// The 5.0/MG status byte is inner[4] (payload byte 1 relative to the oracle's `inner[3..]`
/// payload window), not inner[3]. This vector plants a decoy 0x01 at inner[3] and the true
/// 0x02 at inner[4]: a decoder reading the old offset reports `Ok` instead of `Pending`.
#[test]
fn gen5_status_reads_the_byte_after_the_reserved_one() {
    let wire = unhex("aa010c000001e74124032201020000005eecfbf4");
    let payload = decode_frame(Generation::Gen5, &wire).unwrap();
    assert_eq!(
        decode_control(Generation::Gen5, &payload).unwrap(),
        Some(Control::Response {
            origin_seq: 3,
            to_opcode: 34,
            result: ControlResult::Pending,
        })
    );
}

/// A real 5.0 GET_DATA_RANGE reply captured on hardware. Its inner[3] is 0x04 — the old
/// offset decoded `Unknown(4)` and the historical-sync gate never opened.
#[test]
fn real_gen5_data_range_response_gates_ok() {
    let wire = unhex(concat!(
        "aa014c00010032d124f22204010140bb0100f9ba010001bb0100f9ba0100100000",
        "00000002006a00000088ff1d001432b869cc4c00004549596ab83e00004549596a",
        "b83e0000ae49596aeb1100000000d0da9256",
    ));
    let payload = decode_frame(Generation::Gen5, &wire).unwrap();
    assert_eq!(
        payload[3], 0x04,
        "the byte the old decoder mistook for status"
    );
    assert_eq!(
        decode_control(Generation::Gen5, &payload).unwrap(),
        Some(Control::Response {
            origin_seq: 0xf2,
            to_opcode: 34,
            result: ControlResult::Ok,
        })
    );
}

/// 4.0 exposes no fixed result offset, so the generation reports the fact instead of
/// inventing a code from whichever byte happens to sit there.
#[test]
fn gen4_response_reports_no_status() {
    let wire = unhex("aa0800a8240716020b25cdf9");
    let payload = decode_frame(Generation::Gen4, &wire).unwrap();
    assert_eq!(
        decode_control(Generation::Gen4, &payload).unwrap(),
        Some(Control::Response {
            origin_seq: 7,
            to_opcode: 22,
            result: ControlResult::Unreported,
        })
    );
}

/// A 5.0 response short enough to lack inner[4] is truncated, never silently `Unknown(0)`.
#[test]
fn gen5_response_without_a_status_byte_is_truncated() {
    let wire = unhex("aa0108000001e671240322019a2fe7dc");
    let payload = decode_frame(Generation::Gen5, &wire).unwrap();
    assert_eq!(
        decode_control(Generation::Gen5, &payload).unwrap_err(),
        ProtocolError::Truncated
    );
}

#[test]
fn maverick_metadata_boundaries_decode_exactly() {
    let start = unhex("aa0108000100262131090100164a197f");
    let complete = unhex("aa01080001002621310c03007feae44b");
    assert_eq!(
        decode_control(
            Generation::Gen5,
            &decode_frame(Generation::Gen5, &start).unwrap()
        )
        .unwrap(),
        Some(Control::MetadataStart { seq: 9 })
    );
    assert_eq!(
        decode_control(
            Generation::Gen5,
            &decode_frame(Generation::Gen5, &complete).unwrap()
        )
        .unwrap(),
        Some(Control::MetadataComplete { seq: 12 })
    );
}

#[test]
fn whoop_rs_history_cursor_is_extracted_and_echoed_exactly() {
    let wire = unhex("aa011c00010023d1319102b949596a705d3b000000fdba010010000000000000f269faec");
    let payload = decode_frame(Generation::Gen5, &wire).unwrap();
    let cursor = [0xfd, 0xba, 0x01, 0x00, 0x10, 0x00, 0x00, 0x00];
    assert_eq!(
        decode_control(Generation::Gen5, &payload).unwrap(),
        Some(Control::MetadataEnd { seq: 145, cursor })
    );

    let ack = history_ack(Generation::Gen5, 146, cursor).unwrap();
    let ack_payload = decode_frame(Generation::Gen5, &ack).unwrap();
    assert_eq!(
        &ack_payload[..12],
        &[0x23, 146, 23, 1, 0xfd, 0xba, 1, 0, 16, 0, 0, 0]
    );
}

#[test]
fn whoop_rs_records_route_by_generation_and_version() {
    let gen5_wire = include_str!("fixtures/whoop_rs_gen5_v18.hex").trim();
    let gen4_wire = include_str!("fixtures/whoop_rs_gen4_v24.hex").trim();
    let gen5_payload = decode_frame(Generation::Gen5, &unhex(gen5_wire)).unwrap();
    let gen4_payload = decode_frame(Generation::Gen4, &unhex(gen4_wire)).unwrap();

    assert_eq!(
        classify_record(Generation::Gen5, &gen5_payload).unwrap(),
        RecordDecoder::Gen5V18
    );
    assert_eq!(
        classify_record(Generation::Gen4, &gen4_payload).unwrap(),
        RecordDecoder::Gen4V24
    );

    let mut unmapped = gen5_payload;
    unmapped[1] = 27;
    assert_eq!(
        classify_record(Generation::Gen5, &unmapped).unwrap(),
        RecordDecoder::Unmapped(27)
    );
}

#[test]
fn remaining_whoop_rs_record_goldens_route_exactly() {
    let cases = [
        (
            Generation::Gen5,
            include_str!("fixtures/whoop_rs_gen5_v26.hex").trim(),
            RecordDecoder::Gen5V26,
        ),
        (
            Generation::Gen4,
            include_str!("fixtures/whoop_rs_gen4_v25.hex").trim(),
            RecordDecoder::Gen4V25,
        ),
    ];
    for (generation, wire, expected) in cases {
        let payload = decode_frame(generation, &unhex(wire)).unwrap();
        assert_eq!(classify_record(generation, &payload).unwrap(), expected);
    }
}

#[test]
fn offload_commands_preserve_generation_specific_revision_bytes() {
    let cursor = [1, 2, 3, 4, 5, 6, 7, 8];
    let gen4 = history_ack(Generation::Gen4, 4, cursor).unwrap();
    let gen5 = history_ack(Generation::Gen5, 4, cursor).unwrap();
    // Both generations echo the eight-byte cursor behind the acknowledged-revision marker.
    assert_eq!(
        decode_frame(Generation::Gen4, &gen4).unwrap(),
        [0x23, 4, 23, 1, 1, 2, 3, 4, 5, 6, 7, 8]
    );
    assert_eq!(
        &decode_frame(Generation::Gen5, &gen5).unwrap()[..12],
        &[0x23, 4, 23, 1, 1, 2, 3, 4, 5, 6, 7, 8]
    );
    // Both generations carry the b3 argument byte; an empty gen4 body draws silence.
    assert_eq!(
        decode_frame(
            Generation::Gen4,
            &get_data_range(Generation::Gen4, 5).unwrap()
        )
        .unwrap(),
        [0x23, 5, 34, 0]
    );
    assert_eq!(
        decode_frame(
            Generation::Gen4,
            &request_history(Generation::Gen4, 6).unwrap()
        )
        .unwrap(),
        [0x23, 6, 22, 0]
    );
    assert_eq!(
        decode_frame(
            Generation::Gen5,
            &request_history(Generation::Gen5, 6).unwrap()
        )
        .unwrap(),
        [0x23, 6, 22, 0]
    );
    assert_eq!(
        build_command(Generation::Gen5, 7, 25, &[0]).unwrap_err(),
        ProtocolError::ForbiddenCommand
    );
}

#[test]
fn malformed_inputs_fail_with_exact_errors() {
    assert_eq!(
        decode_frame(Generation::Gen5, &[0xaa, 1]).unwrap_err(),
        ProtocolError::Truncated
    );

    let mut bad_header = unhex("aa0108000001e67123019101363e5c8d");
    bad_header[6] ^= 1;
    assert_eq!(
        decode_frame(Generation::Gen5, &bad_header).unwrap_err(),
        ProtocolError::HeaderCrc
    );

    let mut bad_payload = unhex("aa0800a823070a015511641b");
    bad_payload[4] ^= 1;
    assert_eq!(
        decode_frame(Generation::Gen4, &bad_payload).unwrap_err(),
        ProtocolError::PayloadCrc
    );

    assert_eq!(
        decode_control(Generation::Gen5, &[0x31, 9, 2, 0]).unwrap_err(),
        ProtocolError::Truncated
    );
    assert_eq!(
        classify_record(Generation::Gen5, &[0x28, 18, 0]).unwrap_err(),
        ProtocolError::NotHistoricalRecord
    );
}

/// The v21 IMU deep buffer is identified by its shape, not its version byte. A version-byte
/// collision must not hide it behind a shorter decoder.
#[test]
fn a_gen5_imu_buffer_classifies_structurally_despite_its_version_byte() {
    let mut payload = vec![0u8; 1_232];
    payload[0] = 0x2f;
    payload[1] = 18; // the v18 version byte, on an IMU-shaped buffer
    payload[16..18].copy_from_slice(&100u16.to_le_bytes());
    payload[622..624].copy_from_slice(&100u16.to_le_bytes());
    assert_eq!(
        classify_record(Generation::Gen5, &payload).unwrap(),
        RecordDecoder::Gen5V21
    );

    // Break one count and the version byte decides again.
    payload[622..624].copy_from_slice(&99u16.to_le_bytes());
    assert_eq!(
        classify_record(Generation::Gen5, &payload).unwrap(),
        RecordDecoder::Gen5V18
    );
}

#[test]
fn response_bodies_decode_per_generation() {
    use whoop_protocol::{decode_response, CommandResponse};

    // Gen5 battery is a whole percent at inner 5; gen4 is deci-percent at inner 5..7.
    let mut gen5 = vec![0x24, 1, 26, 0, 1, 93];
    assert_eq!(
        decode_response(Generation::Gen5, &gen5).unwrap(),
        CommandResponse::Battery { deci_percent: 930 }
    );
    gen5.truncate(5);
    assert_eq!(
        decode_response(Generation::Gen5, &gen5).unwrap_err(),
        ProtocolError::Truncated
    );

    let mut gen4 = vec![0x24, 1, 26, 0, 0];
    gen4.extend_from_slice(&812u16.to_le_bytes());
    assert_eq!(
        decode_response(Generation::Gen4, &gen4).unwrap(),
        CommandResponse::Battery { deci_percent: 812 }
    );

    let mut clock = vec![0x24, 1, 11, 0, 0];
    clock.extend_from_slice(&1_784_236_462u32.to_le_bytes());
    assert_eq!(
        decode_response(Generation::Gen4, &clock).unwrap(),
        CommandResponse::Clock {
            unix: 1_784_236_462
        }
    );

    // An opcode with no reviewed decoder says so instead of guessing.
    assert_eq!(
        decode_response(Generation::Gen5, &[0x24, 1, 7, 0, 1]).unwrap(),
        CommandResponse::Unmapped { to_opcode: 7 }
    );
}

/// The banked-history window comes from the real capture, not a fixed offset: the newest word does
/// not sit on the four-byte grid the oldest scan walks.
#[test]
fn the_real_data_range_capture_pins_both_history_bounds() {
    use whoop_protocol::{decode_response, CommandResponse};

    let wire = unhex(concat!(
        "aa014c00010032d124f22204010140bb0100f9ba010001bb0100f9ba0100100000",
        "00000002006a00000088ff1d001432b869cc4c00004549596ab83e00004549596a",
        "b83e0000ae49596aeb1100000000d0da9256",
    ));
    let payload = decode_frame(Generation::Gen5, &wire).unwrap();
    assert_eq!(
        decode_response(Generation::Gen5, &payload).unwrap(),
        CommandResponse::DataRange {
            oldest: Some(1_778_385_408),
            newest: Some(1_784_236_462),
        }
    );
}

#[test]
fn gen5_hello_reads_the_serial_and_gates_the_firmware() {
    use whoop_protocol::{decode_response, CommandResponse};

    let mut payload = vec![0u8; 3 + 100];
    payload[0] = 0x24;
    payload[2] = 145;
    payload[3 + 16..3 + 26].copy_from_slice(b"5AG0546409");
    payload[3 + 93..3 + 97].copy_from_slice(&[50, 40, 1, 0]);
    assert_eq!(
        decode_response(Generation::Gen5, &payload).unwrap(),
        CommandResponse::Hello {
            device_name: "5AG0546409".to_owned(),
            firmware: Some([50, 40, 1, 0]),
        }
    );

    // A non-5.x marker drops the firmware without losing the serial.
    payload[3 + 93] = 40;
    assert_eq!(
        decode_response(Generation::Gen5, &payload).unwrap(),
        CommandResponse::Hello {
            device_name: "5AG0546409".to_owned(),
            firmware: None,
        }
    );
}

/// The destructive tier is refused outright; the gated tier is refused on the general builder and
/// reachable only through the builder that names what it writes.
#[test]
fn the_opcode_policy_has_two_tiers() {
    for opcode in whoop_protocol::DESTRUCTIVE {
        assert_eq!(
            build_command(Generation::Gen5, 1, opcode, &[0]).unwrap_err(),
            ProtocolError::ForbiddenCommand,
            "destructive opcode {opcode} must be refused"
        );
    }
    for opcode in whoop_protocol::GATED {
        assert_eq!(
            build_command(Generation::Gen5, 1, opcode, &[0]).unwrap_err(),
            ProtocolError::ForbiddenCommand,
            "gated opcode {opcode} must not come through the general builder"
        );
    }
    // SET_CONFIG is gated, and its own builder still produces the R22 write.
    let wire = whoop_protocol::set_config(1, "enable_r22_packets", b'2').unwrap();
    let payload = decode_frame(Generation::Gen5, &wire).unwrap();
    assert_eq!(payload[2], 120);
    assert_eq!(payload[3], 1);
    assert_eq!(&payload[4..22], b"enable_r22_packets");
    assert_eq!(payload[36], b'2');
    // A name past its field is refused rather than silently truncated.
    assert!(whoop_protocol::set_config(1, &"x".repeat(33), b'1').is_err());
}

#[test]
fn the_gen5_battery_event_carries_millivolts_and_the_charging_flag() {
    use whoop_protocol::decode_battery_event;

    let mut payload = vec![0u8; 24];
    payload[13..15].copy_from_slice(&812u16.to_le_bytes());
    payload[17..19].copy_from_slice(&3_912u16.to_le_bytes());
    payload[22] = 1;
    let event = decode_battery_event(&payload).unwrap();
    assert_eq!(event.soc_deci_percent, 812);
    assert_eq!(event.millivolts, 3_912);
    assert!(event.charging);

    payload.truncate(14);
    assert_eq!(
        decode_battery_event(&payload).unwrap_err(),
        ProtocolError::Truncated
    );
}
