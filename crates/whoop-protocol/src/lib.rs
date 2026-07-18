#![no_std]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

const START_OF_FRAME: u8 = 0xaa;
const MAX_FRAME_BYTES: usize = 8192;
const TRAILER_BYTES: usize = 4;
const COMMAND: u8 = 0x23;
const COMMAND_RESPONSE: u8 = 0x24;
const HISTORICAL_DATA: u8 = 0x2f;
const METADATA: u8 = 0x31;

/// WHOOP wire generation. The generations share their inner packet header but not their envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Generation {
    Gen4,
    Gen5,
}

impl Generation {
    const fn header_len(self) -> usize {
        match self {
            Self::Gen4 => 4,
            Self::Gen5 => 8,
        }
    }
}

/// Stable failures produced by the pure protocol boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    Truncated,
    Oversized,
    InvalidStart,
    InvalidLength,
    HeaderCrc,
    PayloadCrc,
    NotHistoricalRecord,
    ForbiddenCommand,
}

/// Result byte carried by a command response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlResult {
    Ok,
    Pending,
    Unknown(u8),
}

/// Adjudicated control facts used by historical offload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Control {
    Response {
        origin_seq: u8,
        to_opcode: u8,
        result: ControlResult,
    },
    MetadataStart {
        seq: u8,
    },
    MetadataEnd {
        seq: u8,
        cursor: [u8; 8],
    },
    MetadataComplete {
        seq: u8,
    },
    MetadataUnknown {
        seq: u8,
        kind: u8,
    },
}

/// Reviewed record decoder selected by generation and the version at inner byte 1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordDecoder {
    Gen4V5,
    Gen4V24,
    Gen4V25,
    Gen5V18,
    Gen5V20,
    Gen5V21,
    Gen5V26,
    Unmapped(u8),
}

/// CRC-8, polynomial 0x07 and initial value 0x00.
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x07
            };
        }
    }
    crc
}

/// CRC-16/Modbus, polynomial 0xA001 and initial value 0xFFFF.
pub fn crc16_modbus(data: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for &byte in data {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xa001
            };
        }
    }
    crc
}

/// Reflected zlib CRC-32.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xedb8_8320
            };
        }
    }
    !crc
}

/// Validate exactly one complete wire frame and return its inner payload, including gen5 padding.
pub fn decode_frame(generation: Generation, wire: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let header_len = generation.header_len();
    if wire.len() < header_len {
        return Err(ProtocolError::Truncated);
    }
    if wire[0] != START_OF_FRAME {
        return Err(ProtocolError::InvalidStart);
    }

    let declared = match generation {
        Generation::Gen4 => usize::from(u16::from_le_bytes([wire[1], wire[2]])),
        Generation::Gen5 => usize::from(u16::from_le_bytes([wire[2], wire[3]])),
    };
    if declared < TRAILER_BYTES {
        return Err(ProtocolError::InvalidLength);
    }
    let total = header_len
        .checked_add(declared)
        .ok_or(ProtocolError::Oversized)?;
    if total > MAX_FRAME_BYTES {
        return Err(ProtocolError::Oversized);
    }
    if wire.len() < total {
        return Err(ProtocolError::Truncated);
    }
    if wire.len() != total {
        return Err(ProtocolError::InvalidLength);
    }

    let header_ok = match generation {
        Generation::Gen4 => crc8(&wire[1..3]) == wire[3],
        Generation::Gen5 => u16::from_le_bytes([wire[6], wire[7]]) == crc16_modbus(&wire[..6]),
    };
    if !header_ok {
        return Err(ProtocolError::HeaderCrc);
    }

    let payload_end = total - TRAILER_BYTES;
    let payload = &wire[header_len..payload_end];
    if matches!(generation, Generation::Gen5) && !payload.len().is_multiple_of(4) {
        return Err(ProtocolError::InvalidLength);
    }
    let expected = u32::from_le_bytes([
        wire[payload_end],
        wire[payload_end + 1],
        wire[payload_end + 2],
        wire[payload_end + 3],
    ]);
    if crc32(payload) != expected {
        return Err(ProtocolError::PayloadCrc);
    }
    Ok(payload.to_vec())
}

/// Wrap one inner command in a complete outbound frame.
pub fn build_command(
    generation: Generation,
    seq: u8,
    opcode: u8,
    body: &[u8],
) -> Result<Vec<u8>, ProtocolError> {
    if opcode == 25 {
        return Err(ProtocolError::ForbiddenCommand);
    }
    let body_len = 3usize
        .checked_add(body.len())
        .ok_or(ProtocolError::Oversized)?;
    let mut payload = Vec::with_capacity(body_len);
    payload.extend_from_slice(&[COMMAND, seq, opcode]);
    payload.extend_from_slice(body);
    build_frame(generation, &payload)
}

/// Build GET_DATA_RANGE. The leading zero is confirmed for gen5 and absent from gen4's manifest.
pub fn get_data_range(generation: Generation, seq: u8) -> Result<Vec<u8>, ProtocolError> {
    match generation {
        Generation::Gen4 => build_command(generation, seq, 34, &[]),
        Generation::Gen5 => build_command(generation, seq, 34, &[0]),
    }
}

/// Build SEND_HISTORICAL_DATA without exposing the destructive FORCE_TRIM opcode.
pub fn request_history(generation: Generation, seq: u8) -> Result<Vec<u8>, ProtocolError> {
    match generation {
        Generation::Gen4 => build_command(generation, seq, 22, &[]),
        Generation::Gen5 => build_command(generation, seq, 22, &[0]),
    }
}

/// Echo the eight-byte HISTORY_END cursor, prefixed by gen5's acknowledged revision byte.
pub fn history_ack(
    generation: Generation,
    seq: u8,
    cursor: [u8; 8],
) -> Result<Vec<u8>, ProtocolError> {
    match generation {
        Generation::Gen4 => build_command(generation, seq, 23, &cursor),
        Generation::Gen5 => {
            let mut body = [0u8; 9];
            body[0] = 1;
            body[1..].copy_from_slice(&cursor);
            build_command(generation, seq, 23, &body)
        }
    }
}

/// Decode a command response or offload metadata packet. Data packets return `None`.
pub fn decode_control(payload: &[u8]) -> Result<Option<Control>, ProtocolError> {
    let Some(&packet_type) = payload.first() else {
        return Err(ProtocolError::Truncated);
    };
    match packet_type {
        COMMAND_RESPONSE => {
            let [_, origin_seq, to_opcode, result, ..] = payload else {
                return Err(ProtocolError::Truncated);
            };
            let result = match *result {
                1 => ControlResult::Ok,
                2 => ControlResult::Pending,
                other => ControlResult::Unknown(other),
            };
            Ok(Some(Control::Response {
                origin_seq: *origin_seq,
                to_opcode: *to_opcode,
                result,
            }))
        }
        METADATA => decode_metadata(payload).map(Some),
        _ => Ok(None),
    }
}

fn decode_metadata(payload: &[u8]) -> Result<Control, ProtocolError> {
    let [_, seq, kind, body @ ..] = payload else {
        return Err(ProtocolError::Truncated);
    };
    match *kind {
        1 => Ok(Control::MetadataStart { seq: *seq }),
        2 => {
            let bytes = body.get(10..18).ok_or(ProtocolError::Truncated)?;
            let mut cursor = [0u8; 8];
            cursor.copy_from_slice(bytes);
            Ok(Control::MetadataEnd { seq: *seq, cursor })
        }
        3 => Ok(Control::MetadataComplete { seq: *seq }),
        other => Ok(Control::MetadataUnknown {
            seq: *seq,
            kind: other,
        }),
    }
}

/// Select the reviewed decoder for a validated historical record. Unmapped bytes remain explicit.
pub fn classify_record(
    generation: Generation,
    payload: &[u8],
) -> Result<RecordDecoder, ProtocolError> {
    let [packet_type, version, ..] = payload else {
        return Err(ProtocolError::Truncated);
    };
    if *packet_type != HISTORICAL_DATA {
        return Err(ProtocolError::NotHistoricalRecord);
    }
    Ok(match (generation, *version) {
        (Generation::Gen4, 5 | 7 | 9) => RecordDecoder::Gen4V5,
        (Generation::Gen4, 12 | 24) => RecordDecoder::Gen4V24,
        (Generation::Gen4, 25) => RecordDecoder::Gen4V25,
        (Generation::Gen5, 18) => RecordDecoder::Gen5V18,
        (Generation::Gen5, 20) => RecordDecoder::Gen5V20,
        (Generation::Gen5, 21) => RecordDecoder::Gen5V21,
        (Generation::Gen5, 26) => RecordDecoder::Gen5V26,
        (_, other) => RecordDecoder::Unmapped(other),
    })
}

fn build_frame(generation: Generation, payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let pad = match generation {
        Generation::Gen4 => 1,
        Generation::Gen5 => 4,
    };
    let padded_len = payload
        .len()
        .checked_add(pad - 1)
        .ok_or(ProtocolError::Oversized)?
        / pad
        * pad;
    let declared = padded_len
        .checked_add(TRAILER_BYTES)
        .ok_or(ProtocolError::Oversized)?;
    let declared_u16 = u16::try_from(declared).map_err(|_| ProtocolError::Oversized)?;
    let header_len = generation.header_len();
    let total = header_len
        .checked_add(declared)
        .ok_or(ProtocolError::Oversized)?;
    if total > MAX_FRAME_BYTES {
        return Err(ProtocolError::Oversized);
    }

    let mut wire = match generation {
        Generation::Gen4 => vec![START_OF_FRAME, 0, 0, 0],
        Generation::Gen5 => vec![START_OF_FRAME, 1, 0, 0, 0, 1, 0, 0],
    };
    let length = declared_u16.to_le_bytes();
    match generation {
        Generation::Gen4 => {
            wire[1..3].copy_from_slice(&length);
            wire[3] = crc8(&wire[1..3]);
        }
        Generation::Gen5 => {
            wire[2..4].copy_from_slice(&length);
            let header_crc = crc16_modbus(&wire[..6]).to_le_bytes();
            wire[6..8].copy_from_slice(&header_crc);
        }
    }
    wire.extend_from_slice(payload);
    wire.resize(header_len + padded_len, 0);
    let trailer = crc32(&wire[header_len..]).to_le_bytes();
    wire.extend_from_slice(&trailer);
    Ok(wire)
}
