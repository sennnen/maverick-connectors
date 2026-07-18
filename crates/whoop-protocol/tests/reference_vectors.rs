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

#[test]
fn command_responses_from_both_generations_decode() {
    let gen4 = unhex("aa0800a8240716020b25cdf9");
    let gen5 = unhex("aa01080001002621240322019a2fe7dc");

    let gen4_payload = decode_frame(Generation::Gen4, &gen4).unwrap();
    let gen5_payload = decode_frame(Generation::Gen5, &gen5).unwrap();
    assert_eq!(
        decode_control(&gen4_payload).unwrap(),
        Some(Control::Response {
            origin_seq: 7,
            to_opcode: 22,
            result: ControlResult::Pending,
        })
    );
    assert_eq!(
        decode_control(&gen5_payload).unwrap(),
        Some(Control::Response {
            origin_seq: 3,
            to_opcode: 34,
            result: ControlResult::Ok,
        })
    );
}

#[test]
fn maverick_metadata_boundaries_decode_exactly() {
    let start = unhex("aa0108000100262131090100164a197f");
    let complete = unhex("aa01080001002621310c03007feae44b");
    assert_eq!(
        decode_control(&decode_frame(Generation::Gen5, &start).unwrap()).unwrap(),
        Some(Control::MetadataStart { seq: 9 })
    );
    assert_eq!(
        decode_control(&decode_frame(Generation::Gen5, &complete).unwrap()).unwrap(),
        Some(Control::MetadataComplete { seq: 12 })
    );
}

#[test]
fn whoop_rs_history_cursor_is_extracted_and_echoed_exactly() {
    let wire = unhex("aa011c00010023d1319102b949596a705d3b000000fdba010010000000000000f269faec");
    let payload = decode_frame(Generation::Gen5, &wire).unwrap();
    let cursor = [0xfd, 0xba, 0x01, 0x00, 0x10, 0x00, 0x00, 0x00];
    assert_eq!(
        decode_control(&payload).unwrap(),
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
    assert_eq!(
        decode_frame(Generation::Gen4, &gen4).unwrap(),
        [0x23, 4, 23, 1, 2, 3, 4, 5, 6, 7, 8]
    );
    assert_eq!(
        &decode_frame(Generation::Gen5, &gen5).unwrap()[..12],
        &[0x23, 4, 23, 1, 1, 2, 3, 4, 5, 6, 7, 8]
    );
    assert_eq!(
        decode_frame(
            Generation::Gen4,
            &get_data_range(Generation::Gen4, 5).unwrap()
        )
        .unwrap(),
        [0x23, 5, 34]
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
        decode_control(&[0x31, 9, 2, 0]).unwrap_err(),
        ProtocolError::Truncated
    );
    assert_eq!(
        classify_record(Generation::Gen5, &[0x28, 18, 0]).unwrap_err(),
        ProtocolError::NotHistoricalRecord
    );
}
