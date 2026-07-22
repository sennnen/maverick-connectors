//! COMMAND_RESPONSE body decoders. Offsets below are inner-relative: the inner payload is
//! `[0x24][origin_seq][to_opcode][reserved][status]…`, so a body field at "payload N" in the
//! upstream survey sits at inner `3 + N`.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{Generation, ProtocolError};

const GET_BATTERY_LEVEL: u8 = 26;
const GET_CLOCK_GEN4: u8 = 11;
const GET_DATA_RANGE: u8 = 34;
const GET_HELLO_GEN5: u8 = 145;
const GET_HELLO_GEN4: u8 = 35;

/// The plausible unix window a banked-history word must fall in (≈2023-11 .. 2030-03). A word
/// outside it is not a timestamp, whatever offset it sits at.
const PLAUSIBLE_LO: u32 = 1_700_000_000;
const PLAUSIBLE_HI: u32 = 1_900_000_000;

/// The decoded body of a command response. `Unmapped` is explicit: an opcode with no reviewed
/// decoder is reported, never guessed at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandResponse {
    /// Deci-percent on both generations. Gen5 sends whole percent on the wire; it is scaled here
    /// so one unit reaches the caller.
    Battery {
        deci_percent: u16,
    },
    Clock {
        unix: u32,
    },
    Hello {
        device_name: String,
        firmware: Option<[u8; 4]>,
    },
    /// The banked-history window. Either bound is `None` when no plausible word was found.
    DataRange {
        oldest: Option<u32>,
        newest: Option<u32>,
    },
    Unmapped {
        to_opcode: u8,
    },
}

/// Decode the body of a COMMAND_RESPONSE inner payload.
pub fn decode_response(
    generation: Generation,
    payload: &[u8],
) -> Result<CommandResponse, ProtocolError> {
    let to_opcode = *payload.get(2).ok_or(ProtocolError::Truncated)?;
    let at = |offset: usize| payload.get(3 + offset).copied();
    let u16_at = |offset: usize| {
        payload
            .get(3 + offset..5 + offset)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
    };
    let u32_at = |offset: usize| {
        payload
            .get(3 + offset..7 + offset)
            .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    };

    match (generation, to_opcode) {
        // Gen5 reports whole percent, gen4 deci-percent; both reach the caller as deci-percent.
        (Generation::Gen5, GET_BATTERY_LEVEL) => Ok(CommandResponse::Battery {
            deci_percent: u16::from(at(2).ok_or(ProtocolError::Truncated)?).saturating_mul(10),
        }),
        (Generation::Gen4, GET_BATTERY_LEVEL) => Ok(CommandResponse::Battery {
            deci_percent: u16_at(2).ok_or(ProtocolError::Truncated)?,
        }),
        (Generation::Gen4, GET_CLOCK_GEN4) => Ok(CommandResponse::Clock {
            unix: u32_at(2).ok_or(ProtocolError::Truncated)?,
        }),
        (_, GET_DATA_RANGE) => Ok(CommandResponse::DataRange {
            oldest: scan_oldest(payload),
            newest: scan_newest(payload),
        }),
        (Generation::Gen5, GET_HELLO_GEN5) => Ok(CommandResponse::Hello {
            device_name: ascii_z(payload, 3 + 16),
            // The "5.x" marker gates the firmware block; a truncated block loses the firmware
            // without losing the already-decoded serial.
            firmware: gen5_firmware(payload),
        }),
        (Generation::Gen4, GET_HELLO_GEN4) => {
            let device_name = ascii_z(payload, 3 + 16);
            Ok(CommandResponse::Hello {
                firmware: gen4_firmware(payload, device_name.len()),
                device_name,
            })
        }
        _ => Ok(CommandResponse::Unmapped { to_opcode }),
    }
}

/// The newest plausible banked timestamp, scanning every byte offset: the newest-record word does
/// not sit on a fixed grid.
fn scan_newest(payload: &[u8]) -> Option<u32> {
    let mut newest: Option<u32> = None;
    let mut at = 0;
    while at + 4 <= payload.len() {
        if let Some(word) = plausible_word(payload, at) {
            newest = Some(newest.map_or(word, |seen: u32| seen.max(word)));
        }
        at += 1;
    }
    newest
}

/// The oldest plausible banked timestamp, scanning only the four-byte grid aligned from offset 7.
/// Deliberately asymmetric with the newest scan: an any-offset minimum latches onto a spurious
/// straddle word, while a maximum does not.
fn scan_oldest(payload: &[u8]) -> Option<u32> {
    let mut oldest: Option<u32> = None;
    let mut at = 7;
    while at + 4 <= payload.len() {
        if let Some(word) = plausible_word(payload, at) {
            oldest = Some(oldest.map_or(word, |seen: u32| seen.min(word)));
        }
        at += 4;
    }
    oldest
}

fn plausible_word(payload: &[u8], at: usize) -> Option<u32> {
    let bytes = payload.get(at..at + 4)?;
    let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    (PLAUSIBLE_LO..=PLAUSIBLE_HI)
        .contains(&word)
        .then_some(word)
}

fn gen5_firmware(payload: &[u8]) -> Option<[u8; 4]> {
    let block = payload.get(3 + 93..3 + 97)?;
    (block[0] == 50).then(|| [block[0], block[1], block[2], block[3]])
}

/// The gen4 serial is followed by a variable-length ASCII-hex session token, then a status block
/// whose 4th..7th words are the firmware. Skip the token, then gate on a plausible major.
fn gen4_firmware(payload: &[u8], name_len: usize) -> Option<[u8; 4]> {
    let mut block = 3 + 16 + name_len + 1;
    while payload.get(block).is_some_and(u8::is_ascii_hexdigit) {
        block += 1;
    }
    let word = |at: usize| {
        payload
            .get(at..at + 4)
            .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    };
    let major = word(block + 12).filter(|value| (1..=99).contains(value))?;
    Some([
        major as u8,
        word(block + 16)? as u8,
        word(block + 20)? as u8,
        word(block + 24)? as u8,
    ])
}

/// Printable ASCII from `start` up to a NUL or the first non-printable byte.
fn ascii_z(payload: &[u8], start: usize) -> String {
    let mut text = String::new();
    for &byte in payload.get(start..).unwrap_or(&[]) {
        if byte == 0 || !(32..=126).contains(&byte) {
            break;
        }
        text.push(byte as char);
    }
    text
}

/// The gen5 BATTERY_LEVEL event body: state of charge at inner 13, millivolts at inner 17, and the
/// charging flag in bit 0 of inner 22.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatteryEvent {
    pub soc_deci_percent: u16,
    pub millivolts: u16,
    pub charging: bool,
}

pub fn decode_battery_event(payload: &[u8]) -> Result<BatteryEvent, ProtocolError> {
    let u16_at = |at: usize| {
        payload
            .get(at..at + 2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .ok_or(ProtocolError::Truncated)
    };
    Ok(BatteryEvent {
        soc_deci_percent: u16_at(13)?,
        millivolts: u16_at(17)?,
        charging: payload.get(22).is_some_and(|flags| flags & 1 != 0),
    })
}

/// Every opcode a connector may never send, in either tier.
pub(crate) fn refused(opcode: u8) -> bool {
    DESTRUCTIVE.contains(&opcode) || GATED.contains(&opcode)
}

/// Genuinely destructive: firmware trim and the DFU entries. Never sent, by any path.
pub const DESTRUCTIVE: [u8; 5] = [25, 45, 142, 143, 144];

/// Persistent-state writes. Refused from the general command builder; the legitimate ones are
/// reachable only through their own builders, which state what they write.
pub const GATED: [u8; 9] = [10, 29, 32, 77, 99, 119, 120, 123, 146];

/// Sanity: the two tiers must not overlap, or a caller reading one list draws the wrong conclusion.
#[allow(dead_code)]
const _: () = {
    let mut i = 0;
    while i < DESTRUCTIVE.len() {
        let mut j = 0;
        while j < GATED.len() {
            assert!(DESTRUCTIVE[i] != GATED[j]);
            j += 1;
        }
        i += 1;
    }
};

/// Opcode lists as owned vectors, for callers that want to report the policy.
pub fn refused_opcodes() -> Vec<u8> {
    let mut all = Vec::with_capacity(DESTRUCTIVE.len() + GATED.len());
    all.extend_from_slice(&DESTRUCTIVE);
    all.extend_from_slice(&GATED);
    all
}
