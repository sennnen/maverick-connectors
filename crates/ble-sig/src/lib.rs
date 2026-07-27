//! Bluetooth SIG adopted profiles, decoded. Pure and no-std: bytes in, values out, no transport
//! and no SDK types, so both the generic connector and any vendor connector that also publishes a
//! standard characteristic decode it exactly once.
//!
//! Covered here: the Heart Rate Service (0x180D) with its measurement, body-sensor-location and
//! control-point characteristics, and the Battery Service (0x180F).
#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub const HEART_RATE_SERVICE: &str = "180d";
pub const HEART_RATE_MEASUREMENT: &str = "2a37";
pub const BODY_SENSOR_LOCATION: &str = "2a38";
pub const BATTERY_SERVICE: &str = "180f";
pub const BATTERY_LEVEL: &str = "2a19";

/// Beat-to-beat intervals are published in units of 1/1024 s.
const INTERVAL_UNITS_PER_SECOND: u32 = 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SigError {
    /// The characteristic value stopped before a field its own flags promised.
    Truncated,
}

/// One Heart Rate Measurement notification.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct HeartRateMeasurement {
    pub beats_per_minute: u16,
    /// Whether the sensor reports skin contact, or `None` when it does not claim to know.
    pub skin_contact: Option<bool>,
    /// Beat-to-beat intervals in milliseconds, oldest first, as the profile orders them.
    pub intervals_ms: Vec<u16>,
}

impl HeartRateMeasurement {
    /// Each interval paired with the instant its closing beat happened, reconstructed backwards
    /// from the moment the notification arrived.
    ///
    /// This matters more than it looks. A packet can carry several intervals, and stamping them
    /// all with the arrival time collapses real beats onto one instant — which is exactly the
    /// mistake that makes a variability series treat a burst boundary as a beat-to-beat change.
    /// The intervals are their own clock: the last beat landed at the notification, the one before
    /// it landed an interval earlier, and so on back.
    pub fn timed_intervals(&self, arrived_at_ms: i64) -> Vec<(i64, u16)> {
        let mut timed = Vec::with_capacity(self.intervals_ms.len());
        let mut at = arrived_at_ms;
        for interval in self.intervals_ms.iter().rev() {
            timed.push((at, *interval));
            at = at.saturating_sub(i64::from(*interval));
        }
        timed.reverse();
        timed
    }
}

/// Decode a Heart Rate Measurement characteristic value (0x2A37).
///
/// The flags byte says which fields are present, and every one of them has to be stepped over in
/// order — the intervals sit behind an optional energy-expended field, so reading them at a fixed
/// offset silently decodes the wrong bytes on any sensor that reports calories.
pub fn decode_heart_rate(bytes: &[u8]) -> Result<HeartRateMeasurement, SigError> {
    let &flags = bytes.first().ok_or(SigError::Truncated)?;
    let wide_rate = flags & 0x01 != 0;
    let contact_supported = flags & 0x04 != 0;
    let energy_present = flags & 0x08 != 0;
    let intervals_present = flags & 0x10 != 0;

    let mut at = 1usize;
    let beats_per_minute = if wide_rate {
        u16::from_le_bytes(read(bytes, &mut at)?)
    } else {
        u16::from(u8::from_le_bytes(read(bytes, &mut at)?))
    };
    if energy_present {
        let _kilojoules: [u8; 2] = read(bytes, &mut at)?;
    }

    let mut intervals_ms = Vec::new();
    if intervals_present {
        while at + 2 <= bytes.len() {
            let raw = u32::from(u16::from_le_bytes(read(bytes, &mut at)?));
            if raw > 0 {
                let milliseconds =
                    (raw * 1_000 + INTERVAL_UNITS_PER_SECOND / 2) / INTERVAL_UNITS_PER_SECOND;
                intervals_ms.push(milliseconds as u16);
            }
        }
    }

    Ok(HeartRateMeasurement {
        beats_per_minute,
        skin_contact: contact_supported.then_some(flags & 0x02 != 0),
        intervals_ms,
    })
}

/// Where on the body the sensor sits (0x2A38), which is what decides whether its beats were timed
/// electrically or optically.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SensorSite {
    Other,
    Chest,
    Wrist,
    Finger,
    Hand,
    EarLobe,
    Foot,
}

impl SensorSite {
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Other,
            1 => Self::Chest,
            2 => Self::Wrist,
            3 => Self::Finger,
            4 => Self::Hand,
            5 => Self::EarLobe,
            6 => Self::Foot,
            _ => return None,
        })
    }

    /// True when the intervals this sensor publishes are timed from the heart's electrical signal
    /// rather than from an optical pulse. A chest strap measures the R wave through electrodes; a
    /// wrist, finger, ear or foot sensor measures the pressure pulse some milliseconds later, and
    /// the two are not the same physiological event. Only the first may be called heart-rate
    /// variability, so a sensor that does not say where it is gets the optical answer.
    pub fn is_electrical(self) -> bool {
        matches!(self, Self::Chest)
    }
}

/// Decode a Battery Level characteristic value (0x2A19): one byte, a percentage.
pub fn decode_battery_level(bytes: &[u8]) -> Result<u8, SigError> {
    match bytes.first() {
        Some(&percent) if percent <= 100 => Ok(percent),
        _ => Err(SigError::Truncated),
    }
}

fn read<const N: usize>(bytes: &[u8], at: &mut usize) -> Result<[u8; N], SigError> {
    let field = bytes
        .get(*at..*at + N)
        .and_then(|slice| slice.try_into().ok())
        .ok_or(SigError::Truncated)?;
    *at += N;
    Ok(field)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn a_narrow_rate_with_no_optional_fields_decodes() {
        let decoded = decode_heart_rate(&[0x00, 62]).unwrap();
        assert_eq!(decoded.beats_per_minute, 62);
        assert_eq!(decoded.skin_contact, None);
        assert!(decoded.intervals_ms.is_empty());
    }

    #[test]
    fn a_wide_rate_decodes_little_endian() {
        assert_eq!(
            decode_heart_rate(&[0x01, 0x2C, 0x01])
                .unwrap()
                .beats_per_minute,
            300
        );
    }

    #[test]
    fn contact_is_absent_unless_the_sensor_claims_to_support_it() {
        assert_eq!(decode_heart_rate(&[0x02, 60]).unwrap().skin_contact, None);
        assert_eq!(
            decode_heart_rate(&[0x06, 60]).unwrap().skin_contact,
            Some(true)
        );
        assert_eq!(
            decode_heart_rate(&[0x04, 60]).unwrap().skin_contact,
            Some(false)
        );
    }

    /// The energy-expended field sits between the rate and the intervals. A decoder that assumes a
    /// fixed offset reads two calorie bytes as an interval and publishes nonsense as a heartbeat.
    #[test]
    fn the_optional_energy_field_is_stepped_over_not_assumed_away() {
        // flags 0x18: energy present, intervals present. 500 kJ, then 1024/1024 s = 1000 ms.
        let decoded = decode_heart_rate(&[0x18, 60, 0xF4, 0x01, 0x00, 0x04]).unwrap();
        assert_eq!(decoded.beats_per_minute, 60);
        assert_eq!(decoded.intervals_ms, vec![1_000]);
    }

    #[test]
    fn intervals_convert_from_1024ths_with_rounding() {
        // 819/1024 s = 799.8 ms; 1024/1024 = 1000 ms; a zero is padding and is dropped.
        let decoded = decode_heart_rate(&[0x10, 60, 0x33, 0x03, 0x00, 0x04, 0x00, 0x00]).unwrap();
        assert_eq!(decoded.intervals_ms, vec![800, 1_000]);
    }

    /// Several intervals in one packet are several beats. Sharing the arrival timestamp between
    /// them destroys every gap the variability stage depends on.
    #[test]
    fn intervals_carry_the_instant_their_own_beat_closed() {
        let measurement = HeartRateMeasurement {
            beats_per_minute: 70,
            skin_contact: None,
            intervals_ms: vec![800, 850, 900],
        };
        assert_eq!(
            measurement.timed_intervals(10_000),
            vec![(8_250, 800), (9_100, 850), (10_000, 900)]
        );
    }

    #[test]
    fn a_truncated_value_is_an_error_not_a_guess() {
        assert_eq!(decode_heart_rate(&[]), Err(SigError::Truncated));
        assert_eq!(decode_heart_rate(&[0x01, 0x3C]), Err(SigError::Truncated));
        assert_eq!(
            decode_heart_rate(&[0x08, 60, 0x01]),
            Err(SigError::Truncated)
        );
    }

    /// The distinction the whole project turns on, decided by a byte the sensor publishes about
    /// itself rather than by a guess.
    #[test]
    fn only_a_chest_sensor_times_beats_electrically() {
        assert!(SensorSite::from_code(1).unwrap().is_electrical());
        for optical in [0u8, 2, 3, 4, 5, 6] {
            assert!(!SensorSite::from_code(optical).unwrap().is_electrical());
        }
        assert_eq!(SensorSite::from_code(7), None);
    }

    #[test]
    fn a_battery_level_is_a_percentage_or_it_is_an_error() {
        assert_eq!(decode_battery_level(&[81]), Ok(81));
        assert_eq!(decode_battery_level(&[0]), Ok(0));
        assert_eq!(decode_battery_level(&[101]), Err(SigError::Truncated));
        assert_eq!(decode_battery_level(&[]), Err(SigError::Truncated));
    }
}
