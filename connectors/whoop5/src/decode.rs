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
        // Packet 40 only. R22_REALTIME (16) is a different shape entirely — battery is a direct u8
        // and HR is a u16 of milli-bpm over ten — and no source pins its offsets, so decoding it
        // with the packet-40 layout published wrong heart rates. It fails closed until a capture
        // pins the fields.
        40 => decode_realtime(payload),
        47 => decode_record(payload),
        48 => decode_event(payload),
        36 | 49 => Ok(Vec::new()),
        other => Err(DecodeError::UnknownPacket(other)),
    }
}

pub fn decode_standard_heart_rate(
    bytes: &[u8],
    wall_time_ms: i64,
) -> Result<Vec<WireSample>, DecodeError> {
    let Some(&flags) = bytes.first() else {
        return Err(DecodeError::Truncated);
    };
    let wide = flags & 1 != 0;
    let heart_bytes = if wide { 2 } else { 1 };
    if bytes.len() < 1 + heart_bytes {
        return Err(DecodeError::Truncated);
    }
    let heart_rate = if wide {
        i64::from(u16::from_le_bytes([bytes[1], bytes[2]]))
    } else {
        i64::from(bytes[1])
    };
    let mut samples = Vec::new();
    if heart_rate > 0 {
        samples.push(sample(
            "heart-rate",
            heart_rate * 1_000_000,
            wall_time_ms,
            0,
            "beats-per-minute",
        ));
    }
    if flags & 0x10 != 0 {
        let mut at = 1 + heart_bytes;
        let mut sequence = 0;
        while let Some(value) = bytes.get(at..at + 2) {
            let rr_1024 = u16::from_le_bytes([value[0], value[1]]);
            if rr_1024 != 0 {
                let milliseconds = (u64::from(rr_1024) * 1000 + 512) / 1024;
                samples.push(sample(
                    "rr-interval",
                    i64::try_from(milliseconds).map_err(|_| DecodeError::Truncated)? * 1_000_000,
                    wall_time_ms,
                    sequence,
                    "milliseconds",
                ));
                sequence += 1;
            }
            at += 2;
        }
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

/// The on-wrist marker the strap stamps on every biometric record at inner `[2]`. Two sources pin
/// `0x80` as worn.
const ON_WRIST_MARKER: u8 = 0x80;

fn decode_record(payload: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    let decoder = classify_record(Generation::Gen5, payload).map_err(|_| DecodeError::Truncated)?;
    let [_, version, marker, body @ ..] = payload else {
        return Err(DecodeError::Truncated);
    };
    let marker = *marker;
    match decoder {
        RecordDecoder::Gen5V18 => decode_v18(body),
        RecordDecoder::Gen5V20 => decode_v20(body),
        RecordDecoder::Gen5V21 => decode_v21(body),
        RecordDecoder::Gen5V26 => decode_v26(body),
        RecordDecoder::Unmapped(_) => Err(DecodeError::UnmappedRecord(*version)),
        _ => Err(DecodeError::UnmappedRecord(*version)),
    }
    .map(|mut samples| {
        // Wear state is otherwise only an edge event (cmd 9/10), so a strap already on the wrist
        // when the session opens never reports itself as worn — the app said "off wrist" while it
        // was being worn and streaming heart rate. Every biometric record carries the marker, so
        // read it from there and get a continuous signal.
        //
        // Only the worn marker is claimed. The other values are not pinned by any source, and
        // asserting "off" from a byte nobody has confirmed is how the wrong answer got shown in the
        // first place; unknown stays absent, and the host's freshness window handles staleness.
        if marker == ON_WRIST_MARKER {
            if let Some(time_ms) = samples.first().and_then(|first| first.device_time_ms) {
                samples.push(sample("wrist-state", 1_000_000, time_ms, 0, "boolean"));
            }
        }
        samples
    })
}

fn decode_v18(body: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    if body.len() < 109 {
        return Err(DecodeError::Truncated);
    }
    let time_ms = i64::from(u32_le(body, 4)) * 1000;
    let mut samples = Vec::new();
    if body[11] != 0 {
        samples.push(sample(
            "heart-rate",
            i64::from(body[11]) * 1_000_000,
            time_ms,
            0,
            "beats-per-minute",
        ));
    }
    push_rr(&mut samples, body, 12, 13, time_ms, 4);
    let gravity = [f32_le(body, 34), f32_le(body, 38), f32_le(body, 42)];
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
    let skin_temp = u16_le(body, 62);
    if (500..4500).contains(&skin_temp) {
        // The gate is centi-degrees (5.00..45.00 C), so the raw scales by 10_000, not 1_000_000.
        // At 1_000_000 the v18 capture published 3057 degrees Celsius instead of 30.57.
        samples.push(sample(
            "skin-temp",
            i64::from(skin_temp) * 10_000,
            time_ms,
            0,
            "degrees-celsius",
        ));
    }
    let spo2 = body[71];
    if (70..=100).contains(&spo2) {
        samples.push(sample(
            "spo2-percent",
            i64::from(spo2) * 1_000_000,
            time_ms,
            0,
            "percent",
        ));
    }
    samples.push(sample(
        "step-count",
        i64::from(u16_le(body, 46)) * 1_000_000,
        time_ms,
        0,
        "count",
    ));
    if body[52] <= 2 {
        samples.push(sample(
            "activity-class",
            i64::from(body[52]) * 1_000_000,
            time_ms,
            0,
            "code",
        ));
    }
    samples.push(sample(
        "sleep-state-raw",
        i64::from((body[70] >> 4) & 3) * 1_000_000,
        time_ms,
        0,
        "code",
    ));
    samples.push(sample(
        "signal-quality",
        i64::from(body[29]) * 1_000_000,
        time_ms,
        0,
        "percent",
    ));
    Ok(samples)
}

fn decode_v26(body: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    if body.len() < 73 {
        return Err(DecodeError::Truncated);
    }
    let time_ms = i64::from(u32_le(body, 4)) * 1000;
    Ok((0..24)
        .map(|sequence| {
            sample(
                "ppg",
                i64::from(i16_le(body, 16 + sequence * 2)) * 1_000_000,
                time_ms,
                sequence as u32,
                "counts",
            )
        })
        .collect())
}

fn decode_v21(body: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    const SAMPLES: usize = 100;
    const ACCEL: [usize; 3] = [17, 217, 417];
    const GYRO: [usize; 3] = [629, 829, 1029];
    const MIN_LEN: usize = 1229;
    if body.len() < MIN_LEN {
        return Err(DecodeError::Truncated);
    }
    if u16_le(body, 13) != 100 || u16_le(body, 619) != 100 {
        return Ok(Vec::new());
    }
    let time_ms = i64::from(u32_le(body, 4)) * 1000;
    let mut samples = Vec::with_capacity(600);
    for (stream, axes, unit) in [
        ("imu", ACCEL, "milli-g"),
        ("gyro", GYRO, "milli-degrees-per-second"),
    ] {
        for index in 0..SAMPLES {
            for (axis, base) in axes.into_iter().enumerate() {
                samples.push(sample(
                    stream,
                    i64::from(i16_le(body, base + index * 2)) * 1_000_000,
                    time_ms,
                    (index * 3 + axis) as u32,
                    unit,
                ));
            }
        }
    }
    Ok(samples)
}

fn decode_v20(body: &[u8]) -> Result<Vec<WireSample>, DecodeError> {
    const CHANNELS: [usize; 6] = [36, 236, 1302, 1502, 1724, 1924];
    const SAMPLES: usize = 25;
    const MIN_LEN: usize = 2024;
    if body.len() < MIN_LEN {
        return Err(DecodeError::Truncated);
    }
    let green = u16_le(body, 17);
    if green == 0 || u16_le(body, 20) != green.wrapping_mul(2) {
        return Ok(Vec::new());
    }
    let time_ms = i64::from(u32_le(body, 4)) * 1000;
    let mut samples = Vec::with_capacity(150);
    for (channel, base) in CHANNELS.into_iter().enumerate() {
        for index in 0..SAMPLES {
            samples.push(sample(
                "optical-raw",
                i64::from(sign_extend_20(u32_le(body, base + index * 4))) * 1_000_000,
                time_ms,
                (channel * SAMPLES + index) as u32,
                "counts",
            ));
        }
    }
    Ok(samples)
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
                "rr-interval",
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

fn sign_extend_20(value: u32) -> i32 {
    ((value << 12) as i32) >> 12
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
