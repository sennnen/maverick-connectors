#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use whoop_protocol::{build_command, Deframer, Generation, ProtocolError};

fn gen5_frame(opcode: u8, body: &[u8]) -> Vec<u8> {
    build_command(Generation::Gen5, 1, opcode, body).unwrap()
}

fn payloads(items: Vec<Result<Vec<u8>, ProtocolError>>) -> Vec<Vec<u8>> {
    items
        .into_iter()
        .map(|item| item.expect("frame decoded"))
        .collect()
}

#[test]
fn a_frame_split_across_notifications_reassembles() {
    let wire = gen5_frame(0x91, &[1]);
    let mut deframer = Deframer::new(Generation::Gen5);
    assert!(deframer.push(&wire[..7]).is_empty());
    assert_eq!(
        payloads(deframer.push(&wire[7..])),
        vec![vec![0x23, 1, 0x91, 1]]
    );
}

#[test]
fn two_packed_frames_in_one_notification_both_emerge() {
    let first = gen5_frame(0x91, &[1]);
    let second = gen5_frame(0x76, &[2]);
    let mut wire = first;
    wire.extend_from_slice(&second);
    let mut deframer = Deframer::new(Generation::Gen5);
    assert_eq!(
        payloads(deframer.push(&wire)),
        vec![vec![0x23, 1, 0x91, 1], vec![0x23, 1, 0x76, 2]]
    );
}

#[test]
fn leading_garbage_resyncs_on_the_start_byte() {
    let mut wire = vec![0x00, 0x11, 0x22];
    wire.extend_from_slice(&gen5_frame(0x91, &[1]));
    let mut deframer = Deframer::new(Generation::Gen5);
    assert_eq!(payloads(deframer.push(&wire)), vec![vec![0x23, 1, 0x91, 1]]);
}

/// A v20 optical frame is roughly 2140 bytes, far past any ATT MTU. Delivered in twenty-byte
/// chunks it must still arrive whole and exactly once.
#[test]
fn a_frame_far_larger_than_one_notification_reassembles_in_chunks() {
    let body = vec![0x5a; 2129];
    let wire = gen5_frame(0x91, &body);
    assert!(wire.len() > 2100, "fixture frame is {} bytes", wire.len());

    let mut deframer = Deframer::new(Generation::Gen5);
    let mut decoded = Vec::new();
    for chunk in wire.chunks(20) {
        decoded.extend(deframer.push(chunk));
    }
    let decoded = payloads(decoded);
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].len(), 3 + body.len());
    assert_eq!(&decoded[0][..3], &[0x23, 1, 0x91]);
    assert_eq!(&decoded[0][3..], &body[..]);
}

/// A frame with a broken payload CRC is reported, never silently dropped, and it does not
/// desynchronise the stream behind it.
#[test]
fn a_corrupt_frame_is_reported_and_the_next_frame_still_decodes() {
    let mut corrupt = gen5_frame(0x91, &[1]);
    corrupt[9] ^= 0xff;
    let mut wire = corrupt;
    wire.extend_from_slice(&gen5_frame(0x76, &[2]));

    let mut deframer = Deframer::new(Generation::Gen5);
    let out = deframer.push(&wire);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], Err(ProtocolError::PayloadCrc));
    assert_eq!(out[1], Ok(vec![0x23, 1, 0x76, 2]));
}

#[test]
fn an_implausible_declared_length_is_treated_as_a_false_start_byte() {
    // 0xaa followed by a declared length of two: shorter than the trailer, so not a frame.
    let mut wire = vec![0xaa, 0x01, 0x02, 0x00, 0x00, 0x01, 0x00, 0x00];
    wire.extend_from_slice(&gen5_frame(0x91, &[1]));
    let mut deframer = Deframer::new(Generation::Gen5);
    assert_eq!(payloads(deframer.push(&wire)), vec![vec![0x23, 1, 0x91, 1]]);
}

#[test]
fn a_declared_length_past_the_frame_ceiling_resyncs() {
    let mut wire = vec![0xaa, 0x01, 0xff, 0xff, 0x00, 0x01, 0x00, 0x00];
    wire.extend_from_slice(&gen5_frame(0x91, &[1]));
    let mut deframer = Deframer::new(Generation::Gen5);
    assert_eq!(payloads(deframer.push(&wire)), vec![vec![0x23, 1, 0x91, 1]]);
}

#[test]
fn reset_drops_a_buffered_partial_frame() {
    let wire = gen5_frame(0x91, &[1]);
    let mut deframer = Deframer::new(Generation::Gen5);
    assert!(deframer.push(&wire[..7]).is_empty());
    deframer.reset();
    // The tail alone is not a frame; without the reset it would have completed the first one.
    assert!(deframer.push(&wire[7..]).is_empty());
    assert_eq!(payloads(deframer.push(&wire)), vec![vec![0x23, 1, 0x91, 1]]);
}

#[test]
fn gen4_frames_reassemble_on_their_own_header_layout() {
    let wire = build_command(Generation::Gen4, 7, 0x07, &[1]).unwrap();
    let mut deframer = Deframer::new(Generation::Gen4);
    assert!(deframer.push(&wire[..3]).is_empty());
    assert_eq!(
        payloads(deframer.push(&wire[3..])),
        vec![vec![0x23, 7, 0x07, 1]]
    );
}
