use mav_connector_sdk::abi::WireSample;
use whoop_protocol::{classify_record, Generation, RecordDecoder};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    Truncated,
    UnknownPacket(u8),
    UnmappedRecord(u8),
}

pub fn decode_payload(payload: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    let Some(&packet_type) = payload.first() else {
        return Err(DecodeError::Truncated);
    };
    match packet_type {
        40 => decode_realtime(payload),
        #[cfg(feature = "raw-probe")]
        43 => decode_raw_afe(payload),
        47 => decode_record(payload),
        48 => decode_event(payload),
        36 | 49 => Ok(Vec::new()),
        other => Err(DecodeError::UnknownPacket(other)),
    }
}

// WHOOP 4.0 does carry a raw AFE stream — opcode 63 `[0x01]`, the same trigger as gen5, verified on
// hardware. Its two subtypes differ from gen5: v0a shares gen5's three 100 Hz u16 channel offsets
// (but gen4 has no ECG electrode, so all three are optical), and v0b carries only red + IR at 50 Hz
// with no ambient reference. Probe builds only.
#[cfg(feature = "raw-probe")]
fn decode_raw_afe(payload: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    if let Ok(frame) = whoop_protocol::decode_realtime_raw(payload) {
        return Ok(emit_raw_channels(
            frame.unix_time,
            whoop_protocol::RAW_SAMPLE_RATE_HZ,
            &[
                ("ppg-raw-a", frame.ppg_a.as_slice()),
                ("ppg-raw-b", frame.ecg.as_slice()),
                ("ppg-raw-c", frame.ppg_b.as_slice()),
            ],
        ));
    }
    if let Ok(frame) = whoop_protocol::decode_pulse_ox_gen4(payload) {
        return Ok(emit_raw_channels(
            frame.unix_time,
            whoop_protocol::PULSE_OX_SAMPLE_RATE_HZ_GEN4,
            &[
                ("ppg-red", frame.red.as_slice()),
                ("ppg-ir", frame.ir.as_slice()),
            ],
        ));
    }
    Ok(Vec::new())
}

#[cfg(feature = "raw-probe")]
fn emit_raw_channels<T: Copy + Into<i64>>(
    unix_time: u32,
    rate_hz: u32,
    channels: &[(&str, &[T])],
) -> Vec<WireSample> {
    let base_ms = i64::from(unix_time) * 1000;
    let step_ms = 1000 / i64::from(rate_hz);
    let mut samples = Vec::new();
    for (stream, channel) in channels {
        for (sequence, &counts) in channel.iter().enumerate() {
            samples.push(sample(
                stream,
                counts.into() * 1_000_000,
                base_ms + sequence as i64 * step_ms,
                sequence as u32,
                "counts",
            ));
        }
    }
    samples
}

/// Decode the Bluetooth SIG Heart Rate Measurement characteristic. WHOOP 4.0 publishes the
/// standard profile alongside its own, and the profile is decoded once for everyone in `ble-sig`;
/// what belongs here is only which stream a wrist-worn optical strap's beats go on.
pub fn decode_standard_heart_rate(
    bytes: &[u8],
    wall_time_ms: i64,
) -> Result<Vec<WireSample>, DecodeError> {
    let measurement = ble_sig::decode_heart_rate(bytes).map_err(|_| DecodeError::Truncated)?;
    let mut samples = Vec::new();
    if measurement.beats_per_minute > 0 {
        samples.push(sample(
            "heart-rate",
            i64::from(measurement.beats_per_minute) * 1_000_000,
            wall_time_ms,
            0,
            "beats-per-minute",
        ));
    }
    for (sequence, (at_ms, interval_ms)) in measurement
        .timed_intervals(wall_time_ms)
        .into_iter()
        .enumerate()
    {
        samples.push(sample(
            "pulse-interval",
            i64::from(interval_ms) * 1_000_000,
            at_ms,
            sequence as u32,
            "milliseconds",
        ));
    }
    Ok(samples)
}

fn decode_realtime(payload: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    if payload.len() < 10 {
        return Err(DecodeError::Truncated);
    }
    let time_ms = i64::from(u32_le(payload, 2)) * 1000;
    let mut samples = Vec::new();
    if payload[8] != 0 {
        samples.push(sample(
            "heart-rate",
            i64::from(payload[8]) * 1_000_000,
            time_ms,
            0,
            "beats-per-minute",
        ));
    }
    push_rr(&mut samples, payload, 9, 10, time_ms, u8::MAX);
    Ok(samples)
}

fn decode_record(payload: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    let decoder = classify_record(Generation::Gen4, payload).map_err(|_| DecodeError::Truncated)?;
    let [_, version, _, body @ ..] = payload else {
        return Err(DecodeError::Truncated);
    };
    match decoder {
        RecordDecoder::Gen4V5 => decode_gen4_v5(body),
        RecordDecoder::Gen4V24 => decode_gen4_v24(body),
        RecordDecoder::Gen4V25 => decode_gen4_v25(body),
        RecordDecoder::Unmapped(_) => Err(DecodeError::UnmappedRecord(*version)),
        _ => Err(DecodeError::UnmappedRecord(*version)),
    }
}

fn decode_gen4_v5(body: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    if body.len() < 16 {
        return Err(DecodeError::Truncated);
    }
    let time_ms = i64::from(u32_le(body, 4)) * 1000;
    let mut samples = Vec::new();
    if body[14] != 0 {
        samples.push(sample(
            "heart-rate",
            i64::from(body[14]) * 1_000_000,
            time_ms,
            0,
            "beats-per-minute",
        ));
    }
    push_rr(&mut samples, body, 15, 16, time_ms, 4);
    Ok(samples)
}

fn decode_gen4_v24(body: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    if body.len() < 75 {
        return Err(DecodeError::Truncated);
    }
    let time_ms = i64::from(u32_le(body, 4)) * 1000;
    let mut samples = Vec::new();
    if body[14] != 0 {
        samples.push(sample(
            "heart-rate",
            i64::from(body[14]) * 1_000_000,
            time_ms,
            0,
            "beats-per-minute",
        ));
    }
    push_rr(&mut samples, body, 15, 16, time_ms, 4);
    let gravity = [f32_le(body, 33), f32_le(body, 37), f32_le(body, 41)];
    if gravity_is_plausible(gravity) {
        for (sequence, value) in gravity.into_iter().enumerate() {
            let Some(value_microunits) = f32_microunits(value) else {
                continue;
            };
            samples.push(sample(
                "gravity",
                value_microunits,
                time_ms,
                sequence as u32,
                "milli-g",
            ));
        }
    }
    for (sequence, at) in [61, 63].into_iter().enumerate() {
        samples.push(sample(
            "spo2-raw",
            i64::from(u16_le(body, at)) * 1_000_000,
            time_ms,
            sequence as u32,
            "counts",
        ));
    }
    // 4.0 publishes no calibrated temperature — this is the thermistor register, in counts.
    samples.push(sample(
        "skin-temp-raw",
        i64::from(u16_le(body, 65)) * 1_000_000,
        time_ms,
        0,
        "counts",
    ));
    samples.push(sample(
        "resp-raw",
        i64::from(u16_le(body, 73)) * 1_000_000,
        time_ms,
        0,
        "counts",
    ));
    Ok(samples)
}

fn decode_gen4_v25(body: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    if body.len() < 72 {
        return Err(DecodeError::Truncated);
    }
    let time_ms = i64::from(u32_le(body, 4)) * 1000;
    let gravity = [i16_le(body, 66), i16_le(body, 68), i16_le(body, 70)];
    if !raw_gravity_is_plausible(gravity) {
        return Ok(Vec::new());
    }
    Ok(gravity
        .into_iter()
        .enumerate()
        .map(|(sequence, value)| {
            sample(
                "gravity",
                rounded_ratio(i64::from(value) * 1_000_000, 16_384),
                time_ms,
                sequence as u32,
                "milli-g",
            )
        })
        .collect())
}

fn decode_event(payload: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    if payload.len() < 8 {
        return Err(DecodeError::Truncated);
    }
    let time_ms = i64::from(u32_le(payload, 4)) * 1000;
    match payload[2] {
        3 => {
            if payload.len() < 15 {
                return Err(DecodeError::Truncated);
            }
            let soc_deci = u16_le(payload, 13);
            if soc_deci > 1000 {
                Ok(Vec::new())
            } else {
                Ok(vec![sample(
                    "battery-soc",
                    i64::from(soc_deci) * 100_000,
                    time_ms,
                    0,
                    "percent",
                )])
            }
        }
        9 | 10 => Ok(vec![sample(
            "wrist-state",
            if payload[2] == 9 { 1_000_000 } else { 0 },
            time_ms,
            0,
            "boolean",
        )]),
        _ => Ok(Vec::new()),
    }
}

/// `max_slots` is a layout fact, not a protocol one: the historical records reserve four slots,
/// while a realtime burst carries as many intervals as elapsed since the last one.
fn push_rr(
    samples: &mut Vec<WireSample>,
    bytes: &[u8],
    count_at: usize,
    first_at: usize,
    time_ms: i64,
    max_slots: u8,
) {
    let count = bytes.get(count_at).copied().unwrap_or(0).min(max_slots);
    let mut sequence = 0;
    for slot in 0..usize::from(count) {
        let at = first_at + slot * 2;
        let Some(value) = bytes.get(at..at + 2) else {
            break;
        };
        let rr = u16::from_le_bytes([value[0], value[1]]);
        if rr != 0 {
            samples.push(sample(
                "pulse-interval",
                i64::from(rr) * 1_000_000,
                time_ms,
                sequence,
                "milliseconds",
            ));
            sequence += 1;
        }
    }
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

fn gravity_is_plausible(gravity: [f32; 3]) -> bool {
    if !gravity.iter().all(|value| value.is_finite()) {
        return false;
    }
    let magnitude =
        (gravity[0] * gravity[0] + gravity[1] * gravity[1] + gravity[2] * gravity[2]).sqrt();
    (0.5..1.5).contains(&magnitude)
}

fn raw_gravity_is_plausible(gravity: [i16; 3]) -> bool {
    let squared = gravity
        .into_iter()
        .fold(0i64, |sum, value| sum + i64::from(value) * i64::from(value));
    (8_192i64.pow(2)..24_576i64.pow(2)).contains(&squared)
}

fn f32_microunits(value: f32) -> Option<i64> {
    let bits = value.to_bits();
    let exponent_bits = (bits >> 23) & 0xff;
    if exponent_bits == 0xff {
        return None;
    }
    let mantissa = u64::from(bits & 0x7f_ffff);
    let (significand, exponent) = if exponent_bits == 0 {
        (mantissa, -126)
    } else {
        (
            mantissa | (1 << 23),
            i32::try_from(exponent_bits).ok()? - 127,
        )
    };
    let product = significand.checked_mul(1_000_000)?;
    let binary_shift = exponent - 23;
    let magnitude = if binary_shift >= 0 {
        product.checked_shl(u32::try_from(binary_shift).ok()?)?
    } else {
        let right = u32::try_from(-binary_shift).ok()?;
        if right >= 64 {
            0
        } else {
            let rounding = if right == 0 { 0 } else { 1u64 << (right - 1) };
            product.checked_add(rounding)? >> right
        }
    };
    let signed = i64::try_from(magnitude).ok()?;
    Some(if bits >> 31 == 0 { signed } else { -signed })
}

fn rounded_ratio(numerator: i64, denominator: i64) -> i64 {
    if numerator >= 0 {
        (numerator + denominator / 2) / denominator
    } else {
        (numerator - denominator / 2) / denominator
    }
}

fn u16_le(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn i16_le(bytes: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_le(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn f32_le(bytes: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
