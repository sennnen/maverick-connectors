//! Decode the type-43 `REALTIME_RAW_DATA` frame — the raw analog-front-end (AFE) stream.
//!
//! Cracked on a live WHOOP MG (firmware 50.33.2.0), documented in `docs/protocol/whoop.md`. The
//! stream is started by [`START_AFE_RAW`] (opcode 63) with a `[0x01]` revision byte — *not* by
//! `START_RAW_DATA` (81), which every prior source assumed and which this firmware accepts but
//! leaves silent.
//!
//! The stream emits two interleaved frame subtypes, one pair per second (a Unix timestamp at byte
//! 7 increments once per pair):
//!
//! - **v0x0a** (1920 bytes): three 100-sample little-endian `u16` channels at fixed offsets — two
//!   optical PPG channels and, between them, the single-lead **ECG** electrode channel. 100 samples
//!   per one-second frame ⇒ **100 Hz**. This module decodes it.
//! - **v0x0b** (1924 bytes): three 25-sample little-endian *signed* `i32` channels — the pulse-ox
//!   triad **red + IR + ambient**. 25 samples per one-second frame ⇒ **25 Hz**. Decoded by
//!   [`decode_pulse_ox`].
//!
//! The v0x0a ECG channel was pinned with an electrode-contact control (the middle channel rails
//! between the ADC extremes when the electrode floats and settles to a biopotential on skin). The
//! v0x0b channels were pinned with lighting controls on a worn strap: red and IR are reflective LED
//! PPG (positive on skin, rail negative in open air), ambient is an ambient-light photodiode (floods
//! in open light), and a 940 nm remote moves the IR channel far more than red. See
//! `docs/protocol/whoop-raw-afe.md`.

use crate::ProtocolError;

/// Opcode that starts the raw AFE stream. Sent with a `[0x01]` revision byte.
pub const START_AFE_RAW: u8 = 63;
/// Opcode that stops it (shares the `STOP_RAW_DATA` number).
pub const STOP_AFE_RAW: u8 = 82;

/// Packet type of a raw-AFE frame.
pub const REALTIME_RAW_DATA: u8 = 43;

/// Samples per channel in a v0x0a frame: 100 per one-second frame, i.e. 100 Hz.
pub const RAW_SAMPLES_PER_FRAME: usize = 100;
/// The raw AFE sample rate, in hertz.
pub const RAW_SAMPLE_RATE_HZ: u32 = 100;

/// Samples per channel in a v0x0b frame: 25 per one-second frame, i.e. 25 Hz.
pub const PULSE_OX_SAMPLES_PER_FRAME: usize = 25;
/// The v0x0b pulse-ox sample rate, in hertz.
pub const PULSE_OX_SAMPLE_RATE_HZ: u32 = 25;

const VERSION_V0A: u8 = 0x0a;
const VERSION_V0B: u8 = 0x0b;
const UNIX_OFFSET: usize = 7;
// Fixed byte offsets of the three contiguous 100-sample u16 channels within the v0x0a payload.
const OFF_PPG_A: usize = 0x055;
const OFF_ECG: usize = 0x11d;
const OFF_PPG_B: usize = 0x1e5;
const CHANNEL_BYTES: usize = RAW_SAMPLES_PER_FRAME * 2;
// Fixed byte offsets of the three 25-sample signed-i32 channels within the v0x0b payload.
const OFF_RED: usize = 0x026;
const OFF_IR: usize = 0x0ee;
const OFF_AMBIENT: usize = 0x6b9;
const PULSE_OX_CHANNEL_BYTES: usize = PULSE_OX_SAMPLES_PER_FRAME * 4;

/// One decoded v0x0a raw-AFE frame: three synchronous 100-sample channels at 100 Hz.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawAfeFrame {
    /// Strap Unix time (seconds) the frame's samples span one second from.
    pub unix_time: u32,
    /// Single-lead ECG electrode channel (raw ADC counts).
    pub ecg: [u16; RAW_SAMPLES_PER_FRAME],
    /// First optical PPG channel (raw ADC counts).
    pub ppg_a: [u16; RAW_SAMPLES_PER_FRAME],
    /// Second optical PPG channel (raw ADC counts).
    pub ppg_b: [u16; RAW_SAMPLES_PER_FRAME],
}

/// One decoded v0x0b frame: the pulse-ox triad, three synchronous 25-sample channels at 25 Hz.
///
/// Samples are **signed** — positive on skin, railing to a negative floor in open air for the
/// reflective LED channels. Raw ADC counts; converting them to an SpO2 percentage is a downstream
/// calibration step, not done here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawPulseOxFrame {
    /// Strap Unix time (seconds) the frame's samples span one second from.
    pub unix_time: u32,
    /// Red LED reflective PPG channel (~660 nm).
    pub red: [i32; PULSE_OX_SAMPLES_PER_FRAME],
    /// Infrared LED reflective PPG channel (~940 nm).
    pub ir: [i32; PULSE_OX_SAMPLES_PER_FRAME],
    /// Ambient-light reference photodiode.
    pub ambient: [i32; PULSE_OX_SAMPLES_PER_FRAME],
}

/// Decode a v0x0a raw-AFE frame. Returns `NotHistoricalRecord` for a non-type-43 payload and
/// `InvalidLength` for the v0x0b subtype or a short buffer (both are left undecoded on purpose).
pub fn decode_realtime_raw(payload: &[u8]) -> Result<RawAfeFrame, ProtocolError> {
    if payload.first() != Some(&REALTIME_RAW_DATA) {
        return Err(ProtocolError::NotHistoricalRecord);
    }
    if payload.get(1) != Some(&VERSION_V0A) || payload.len() < OFF_PPG_B + CHANNEL_BYTES {
        return Err(ProtocolError::InvalidLength);
    }
    let unix_time = u32::from_le_bytes([
        payload[UNIX_OFFSET],
        payload[UNIX_OFFSET + 1],
        payload[UNIX_OFFSET + 2],
        payload[UNIX_OFFSET + 3],
    ]);
    Ok(RawAfeFrame {
        unix_time,
        ecg: channel(payload, OFF_ECG),
        ppg_a: channel(payload, OFF_PPG_A),
        ppg_b: channel(payload, OFF_PPG_B),
    })
}

fn channel(payload: &[u8], offset: usize) -> [u16; RAW_SAMPLES_PER_FRAME] {
    let mut out = [0u16; RAW_SAMPLES_PER_FRAME];
    for (i, slot) in out.iter_mut().enumerate() {
        let at = offset + 2 * i;
        *slot = u16::from_le_bytes([payload[at], payload[at + 1]]);
    }
    out
}

/// Decode a v0x0b pulse-ox frame (red + IR + ambient, 25 Hz). Returns `NotHistoricalRecord` for a
/// non-type-43 payload and `InvalidLength` for the v0x0a subtype or a short buffer.
pub fn decode_pulse_ox(payload: &[u8]) -> Result<RawPulseOxFrame, ProtocolError> {
    if payload.first() != Some(&REALTIME_RAW_DATA) {
        return Err(ProtocolError::NotHistoricalRecord);
    }
    if payload.get(1) != Some(&VERSION_V0B) || payload.len() < OFF_AMBIENT + PULSE_OX_CHANNEL_BYTES
    {
        return Err(ProtocolError::InvalidLength);
    }
    let unix_time = u32::from_le_bytes([
        payload[UNIX_OFFSET],
        payload[UNIX_OFFSET + 1],
        payload[UNIX_OFFSET + 2],
        payload[UNIX_OFFSET + 3],
    ]);
    Ok(RawPulseOxFrame {
        unix_time,
        red: channel_i32(payload, OFF_RED),
        ir: channel_i32(payload, OFF_IR),
        ambient: channel_i32(payload, OFF_AMBIENT),
    })
}

fn channel_i32(payload: &[u8], offset: usize) -> [i32; PULSE_OX_SAMPLES_PER_FRAME] {
    let mut out = [0i32; PULSE_OX_SAMPLES_PER_FRAME];
    for (i, slot) in out.iter_mut().enumerate() {
        let at = offset + 4 * i;
        *slot = i32::from_le_bytes([
            payload[at],
            payload[at + 1],
            payload[at + 2],
            payload[at + 3],
        ]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn planted() -> alloc::vec::Vec<u8> {
        // A minimal v0x0a payload: type + version, a timestamp at byte 7, and known ramps planted
        // at the three channel offsets. Mirrors the real 1920-byte frame layout.
        let mut p = vec![0u8; OFF_PPG_B + CHANNEL_BYTES];
        p[0] = REALTIME_RAW_DATA;
        p[1] = VERSION_V0A;
        p[UNIX_OFFSET..UNIX_OFFSET + 4].copy_from_slice(&1_784_834_941u32.to_le_bytes());
        for i in 0..RAW_SAMPLES_PER_FRAME {
            p[OFF_ECG + 2 * i..OFF_ECG + 2 * i + 2]
                .copy_from_slice(&(1200u16 + i as u16).to_le_bytes());
            p[OFF_PPG_A + 2 * i..OFF_PPG_A + 2 * i + 2]
                .copy_from_slice(&(470u16 + i as u16).to_le_bytes());
            p[OFF_PPG_B + 2 * i..OFF_PPG_B + 2 * i + 2]
                .copy_from_slice(&(3900u16 + i as u16).to_le_bytes());
        }
        p
    }

    #[test]
    fn decodes_channels_and_timestamp() {
        let decoded = decode_realtime_raw(&planted());
        assert!(matches!(
            &decoded,
            Ok(f) if f.unix_time == 1_784_834_941
                && f.ecg[0] == 1200
                && f.ecg[99] == 1299
                && f.ppg_a[0] == 470
                && f.ppg_b[0] == 3900
        ));
    }

    #[test]
    fn rejects_wrong_type_and_v0b() {
        assert_eq!(
            decode_realtime_raw(&[0x2f, 0x0a]),
            Err(ProtocolError::NotHistoricalRecord)
        );
        let mut v0b = planted();
        v0b[1] = 0x0b;
        assert_eq!(decode_realtime_raw(&v0b), Err(ProtocolError::InvalidLength));
    }

    fn planted_pulse_ox() -> alloc::vec::Vec<u8> {
        // A minimal v0x0b payload: type + version, timestamp at byte 7, and known signed ramps at
        // the three channel offsets — red goes negative to exercise the signed decode.
        let mut p = vec![0u8; OFF_AMBIENT + PULSE_OX_CHANNEL_BYTES];
        p[0] = REALTIME_RAW_DATA;
        p[1] = VERSION_V0B;
        p[UNIX_OFFSET..UNIX_OFFSET + 4].copy_from_slice(&1_784_834_941u32.to_le_bytes());
        for i in 0..PULSE_OX_SAMPLES_PER_FRAME {
            let put = |p: &mut [u8], off: usize, v: i32| {
                p[off + 4 * i..off + 4 * i + 4].copy_from_slice(&v.to_le_bytes());
            };
            put(&mut p, OFF_RED, -100_000 + i as i32);
            put(&mut p, OFF_IR, 180_000 + i as i32);
            put(&mut p, OFF_AMBIENT, 13_000_000 + i as i32);
        }
        p
    }

    #[test]
    fn decodes_pulse_ox_signed_channels() {
        let decoded = decode_pulse_ox(&planted_pulse_ox());
        assert!(matches!(
            &decoded,
            Ok(f) if f.unix_time == 1_784_834_941
                && f.red[0] == -100_000
                && f.red[24] == -99_976
                && f.ir[0] == 180_000
                && f.ambient[0] == 13_000_000
        ));
    }

    #[test]
    fn pulse_ox_rejects_wrong_type_and_v0a() {
        assert_eq!(
            decode_pulse_ox(&[0x2f, 0x0b]),
            Err(ProtocolError::NotHistoricalRecord)
        );
        // v0x0a payload (version 0x0a) must be refused by the v0x0b decoder.
        assert_eq!(
            decode_pulse_ox(&planted()),
            Err(ProtocolError::InvalidLength)
        );
    }
}
