pub mod decode;

use decode::{decode_payload, decode_standard_heart_rate};
use mav_connector_sdk::abi::*;
use mav_connector_sdk::{
    artifact_metadata, export_connector, ActionBuilder, Connector, ConnectorError, TestDriver,
};
use sha2::{Digest, Sha256};
use whoop_protocol::{
    build_command, decode_control, decode_response, get_data_range, history_ack, request_history,
    CommandResponse, Control, ControlResult, Deframer, Generation,
};

pub const CONNECTOR_ID: &str = "dev.maverick.whoop5";
/// A probe build is different bytes, so it must be a different version — the install policy
/// refuses two artifacts claiming one version, which is exactly right. The 900 series is reserved
/// for discovery builds and never released.
#[cfg(not(feature = "ecg-probe"))]
pub const CONNECTOR_VERSION: &str = "1.0.5";
#[cfg(feature = "ecg-probe")]
pub const CONNECTOR_VERSION: &str = "1.903.0";
pub const GEN5_SERVICE: &str = "fd4b0001-cce1-4033-93ce-002d5875f58a";
const COMMAND_ID: &str = "command";
const STANDARD_HR_ID: &str = "standard-heart-rate";
const COMMAND_RESPONSE_ID: &str = "command-response";
const EVENTS_ID: &str = "events";
const DATA_ID: &str = "data";
/// The strap narrates its firmware console here — history-sync progress, RTC complaints,
/// the persistent-config table. Text, never samples.
const CONSOLE_ID: &str = "console";
/// Packet type 50. Console frames are identified by this, not by which characteristic carried them.
const CONSOLE_LOGS_PACKET: u8 = 50;
const ALL_SUBSCRIPTIONS: u8 = 0x1f;
const IDLE_TIMER: TimerToken = TimerToken(200);
const RESPONSE_TIMER: TimerToken = TimerToken(201);
const IDLE_DELAY_MS: u64 = 60_000;
/// A live sample newer than this keeps the link reserved for streaming, so the periodic offload
/// deadline is pushed out instead of preempting realtime notifications.
const LIVE_ACTIVE_MS: i64 = 10_000;
const SNAPSHOT_LEN: usize = 17;
/// The ordered SET_CONFIG sequence that unlocks the deep biometric streams. Values are the ASCII
/// digits the official app writes, not binary 1 and 2.
const FEATURE_FLAGS: [(&str, u8); 16] = [
    ("enable_r22_packets", b'2'),
    ("enable_r22_v2_packets", b'2'),
    ("enable_r22_v3_packets", b'2'),
    ("enable_r22_v4_packets", b'1'),
    ("enable_r22_v5_packets", b'2'),
    ("enable_r22_v6_packets", b'2'),
    ("enable_r22_v8_packets", b'2'),
    ("make_hrfm_visible", b'2'),
    ("disable_pip_r26_packets", b'2'),
    ("wear_detect_bias", b'2'),
    ("hr_ch_switching", b'2'),
    ("ir_hw_switching", b'2'),
    ("enable_passive_strap_fit_gen5", b'1'),
    ("enable_sig11_during_sleep", b'2'),
    ("dorset_inhibit_wpt", b'2'),
    ("enable_sig12", b'1'),
];
/// Config step of the first SET_CONFIG write; the three steps before it are the opcode 3, 117, and
/// 118 preamble.
const FIRST_FLAG_STEP: u8 = 3;
const LAST_FLAG_STEP: u8 = FIRST_FLAG_STEP + FEATURE_FLAGS.len() as u8 - 1;

/// The ECG discovery steps, appended after the R22 sequence in a probe build only.
///
/// The reasoning, recorded because nobody has decoded this: the MG carries a single-lead ECG that
/// no source has found on the wire, the firmware gates it behind a config key, and the one config
/// flag naming ECG is `enable_raw_data_w_ecg` — present in firmware but absent from the R22
/// sequence. The stream that flag qualifies is opened by `START_RAW_DATA`, and the packet type
/// that stream produces is 43, `REALTIME_RAW_DATA`, which every source names and none decodes.
/// So: set the flag, open the stream, and read whatever type 43 carries.
///
/// Both writes are reversible and neither is destructive. `STOP_RAW_DATA` closes the stream, and
/// the link dropping closes it too.
/// The config key exchange, cracked against a live MG.
///
/// `117 [0x01]` opens it and answers `Ok` with the key count in its body — 14 on firmware 50.33.2.0.
/// `118 [0x01]` then walks the table one key per call, answering `Ok` with that key's **name**
/// (`general_ab_test` came back first, one of the firmware-only flags absent from the R22 set).
///
/// Sent with an empty body — as this connector did until now — the strap reads a revision of zero
/// and refuses both with `unsupported revision:0` on its console. The exchange then never opens, and
/// SET_CONFIG for any key the firmware has not announced is rejected. That is why five R22 flags
/// failed, `enable_raw_data_w_ecg` among them.
#[cfg(feature = "ecg-probe")]
const CONFIG_KEY_REVISION: u8 = 0x01;
/// How many times to call 118. The strap reported 14 keys; a few spare calls cost nothing and the
/// replies say when the table is exhausted.
#[cfg(feature = "ecg-probe")]
const CONFIG_KEY_WALK: u8 = 18;
#[cfg(feature = "ecg-probe")]
const KEY_OPEN_STEP: u8 = LAST_FLAG_STEP + 1;
#[cfg(feature = "ecg-probe")]
const KEY_WALK_FIRST: u8 = KEY_OPEN_STEP + 1;
#[cfg(feature = "ecg-probe")]
const KEY_WALK_LAST: u8 = KEY_WALK_FIRST + CONFIG_KEY_WALK - 1;
#[cfg(feature = "ecg-probe")]
const ECG_FLAG_STEP: u8 = KEY_WALK_LAST + 1;
#[cfg(feature = "ecg-probe")]
const ECG_START_STEP: u8 = ECG_FLAG_STEP + 1;
/// A diagnostic is a host operation, and a session has a bounded budget for them. The first probe
/// build spent it on console narration and died mid-run; this cap keeps the interesting early
/// frames and stops before the session does.
#[cfg(feature = "ecg-probe")]
const PROBE_DIAGNOSTIC_BUDGET: u16 = 150;
#[cfg(feature = "ecg-probe")]
const PROBE_HEX_BYTES: usize = 192;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
enum Phase {
    #[default]
    Idle = 0,
    Scanning = 1,
    Connecting = 2,
    Pairing = 3,
    Discovering = 4,
    Subscribing = 5,
    Configuring = 6,
    Streaming = 7,
    Historical = 8,
    Suspended = 9,
    Disconnected = 10,
}

#[derive(Debug)]
pub struct Whoop5Connector {
    phase: Phase,
    subscriptions: u8,
    command_seq: u8,
    next_operation: u64,
    history_retries: u8,
    config_step: u8,
    paired: bool,
    /// Wall time of the newest live sample. Session-local; deliberately absent from the snapshot so
    /// the state schema and every frozen fixture hash stay unchanged.
    last_live_ms: Option<i64>,
    /// One reassembly buffer per notify characteristic. Also session-local: a frame cut short by a
    /// dropped link must never be resumed across a reconnect.
    deframers: Deframers,
    /// Remaining probe diagnostics this session. Session-local and absent from the snapshot.
    #[cfg(feature = "ecg-probe")]
    probe_budget: u16,
}

/// Frame reassembly for the four framed notify characteristics. Standard heart rate is a Bluetooth
/// SIG characteristic carrying no WHOOP envelope, so it has none.
#[derive(Debug)]
struct Deframers {
    command_response: Deframer,
    events: Deframer,
    data: Deframer,
    console: Deframer,
}

impl Default for Deframers {
    fn default() -> Self {
        Self {
            command_response: Deframer::new(Generation::Gen5),
            events: Deframer::new(Generation::Gen5),
            data: Deframer::new(Generation::Gen5),
            console: Deframer::new(Generation::Gen5),
        }
    }
}

impl Deframers {
    fn get_mut(&mut self, characteristic_id: &str) -> Option<&mut Deframer> {
        match characteristic_id {
            COMMAND_RESPONSE_ID => Some(&mut self.command_response),
            EVENTS_ID => Some(&mut self.events),
            DATA_ID => Some(&mut self.data),
            CONSOLE_ID => Some(&mut self.console),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.command_response.reset();
        self.events.reset();
        self.data.reset();
        self.console.reset();
    }
}

impl Default for Whoop5Connector {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            subscriptions: 0,
            command_seq: 1,
            next_operation: 1,
            history_retries: 0,
            config_step: 0,
            paired: false,
            last_live_ms: None,
            deframers: Deframers::default(),
            #[cfg(feature = "ecg-probe")]
            probe_budget: PROBE_DIAGNOSTIC_BUDGET,
        }
    }
}

impl Connector for Whoop5Connector {
    fn handle(&mut self, event: ConnectorEvent) -> Result<ActionBatch, ConnectorError> {
        match &event.body {
            EventBody::Init { .. } => Ok(empty()),
            EventBody::Activate | EventBody::Resume => {
                self.phase = Phase::Scanning;
                self.subscriptions = 0;
                self.paired = false;
                self.deframers.reset();
                self.actions(
                    &event,
                    vec![ActionBody::StartScan {
                        service_uuids: vec![GEN5_SERVICE.to_owned(), "180d".to_owned()],
                        manufacturer_ids: Vec::new(),
                    }],
                )
            }
            EventBody::Advertisement {
                address,
                service_uuids,
                name,
                ..
            } if self.phase == Phase::Scanning && is_gen5(service_uuids, name.as_deref()) => {
                self.phase = Phase::Connecting;
                self.actions(
                    &event,
                    vec![
                        ActionBody::StopScan,
                        ActionBody::Connect {
                            address: address.clone(),
                        },
                    ],
                )
            }
            EventBody::Advertisement { .. } => Ok(empty()),
            EventBody::Connected { .. } => {
                self.phase = Phase::Pairing;
                self.actions(&event, vec![ActionBody::EnsurePaired])
            }
            EventBody::PairingResult { success: true, .. } => {
                self.paired = true;
                self.phase = Phase::Discovering;
                self.actions(&event, vec![ActionBody::DiscoverServices])
            }
            EventBody::PairingResult { success: false, .. } => {
                self.phase = Phase::Disconnected;
                self.actions(
                    &event,
                    vec![
                        ActionBody::EmitDiagnostic {
                            level: DiagnosticLevel::Error,
                            code: "whoop5-pairing".to_owned(),
                            message: "WHOOP 5.0/MG pairing failed".to_owned(),
                        },
                        ActionBody::Disconnect,
                    ],
                )
            }
            EventBody::ServicesDiscovered { service_uuids }
                if self.paired
                    && has_uuid(service_uuids, GEN5_SERVICE)
                    && has_uuid(service_uuids, "180d") =>
            {
                self.phase = Phase::Subscribing;
                self.actions(
                    &event,
                    [
                        STANDARD_HR_ID,
                        COMMAND_RESPONSE_ID,
                        EVENTS_ID,
                        DATA_ID,
                        CONSOLE_ID,
                    ]
                    .into_iter()
                    .map(|id| ActionBody::Subscribe {
                        characteristic_id: id.to_owned(),
                    })
                    .collect(),
                )
            }
            EventBody::ServicesDiscovered { .. } => self.diagnostic(
                &event,
                DiagnosticLevel::Error,
                "whoop5-services",
                "paired WHOOP 5.0/MG services were not discovered",
            ),
            EventBody::IdentityRead { field_id, bytes } if field_id == "model-number" => {
                let accepted = core::str::from_utf8(bytes).is_ok_and(|model| {
                    matches!(
                        model.trim(),
                        "5.0" | "WHOOP 5.0" | "MG" | "WHOOP MG" | "WHOOP"
                    )
                });
                if accepted {
                    Ok(empty())
                } else {
                    self.phase = Phase::Disconnected;
                    self.actions(
                        &event,
                        vec![
                            ActionBody::EmitDiagnostic {
                                level: DiagnosticLevel::Error,
                                code: "whoop5-identity".to_owned(),
                                message: "model identity does not name WHOOP 5.0 or MG".to_owned(),
                            },
                            ActionBody::Disconnect,
                        ],
                    )
                }
            }
            EventBody::Subscribed { characteristic_id } => {
                if let Some(bit) = subscription_bit(characteristic_id) {
                    self.subscriptions |= bit;
                }
                if self.subscriptions == ALL_SUBSCRIPTIONS && self.phase == Phase::Subscribing {
                    self.phase = Phase::Configuring;
                    self.config_step = 0;
                    let hello = self.command(145, &[1])?;
                    self.write(&event, hello)
                } else {
                    Ok(empty())
                }
            }
            EventBody::WriteResult {
                characteristic_id, ..
            } if characteristic_id == COMMAND_ID && self.phase == Phase::Configuring => {
                self.advance_configuration(&event)
            }
            EventBody::Notification {
                characteristic_id,
                bytes,
            } => self.notification(&event, characteristic_id, bytes),
            EventBody::TimerFired { token } if *token == IDLE_TIMER => {
                if self.live_is_active(event.wall_time_ms) {
                    return self.actions(
                        &event,
                        vec![ActionBody::SetTimer {
                            token: IDLE_TIMER,
                            delay_ms: IDLE_DELAY_MS,
                        }],
                    );
                }
                self.phase = Phase::Historical;
                self.history_retries = 0;
                let seq = self.take_command_seq();
                let command = get_data_range(Generation::Gen5, seq).map_err(protocol_error)?;
                self.write_with_timeout(&event, command)
            }
            EventBody::TimerFired { token } if *token == RESPONSE_TIMER => {
                if self.phase != Phase::Historical || self.history_retries >= 1 {
                    self.phase = Phase::Streaming;
                    return self.actions(
                        &event,
                        vec![
                            ActionBody::EmitDiagnostic {
                                level: DiagnosticLevel::Warning,
                                code: "whoop5-history-timeout".to_owned(),
                                message: "historical response timed out".to_owned(),
                            },
                            ActionBody::SetTimer {
                                token: IDLE_TIMER,
                                delay_ms: IDLE_DELAY_MS,
                            },
                        ],
                    );
                }
                self.history_retries += 1;
                let seq = self.take_command_seq();
                let command = request_history(Generation::Gen5, seq).map_err(protocol_error)?;
                self.write_with_timeout(&event, command)
            }
            EventBody::Disconnected { .. } => {
                self.phase = Phase::Disconnected;
                self.subscriptions = 0;
                self.paired = false;
                self.deframers.reset();
                let snapshot = self.snapshot()?;
                self.actions(
                    &event,
                    vec![
                        ActionBody::StatePut {
                            key: "session".to_owned(),
                            value: snapshot,
                        },
                        ActionBody::StateCommit,
                    ],
                )
            }
            EventBody::RestoreState { bytes } => {
                self.restore(bytes)?;
                self.deframers.reset();
                Ok(empty())
            }
            EventBody::Suspend => {
                self.phase = Phase::Suspended;
                self.actions(
                    &event,
                    vec![
                        ActionBody::CancelTimer { token: IDLE_TIMER },
                        ActionBody::Disconnect,
                    ],
                )
            }
            EventBody::Cancel { .. } | EventBody::Deactivate => {
                self.phase = Phase::Disconnected;
                self.actions(
                    &event,
                    vec![
                        ActionBody::CancelTimer { token: IDLE_TIMER },
                        ActionBody::CancelTimer {
                            token: RESPONSE_TIMER,
                        },
                        ActionBody::Disconnect,
                    ],
                )
            }
            _ => Ok(empty()),
        }
    }

    fn snapshot(&self) -> Result<Vec<u8>, ConnectorError> {
        let mut bytes = Vec::with_capacity(SNAPSHOT_LEN);
        bytes.extend_from_slice(b"W5");
        bytes.push(1);
        bytes.push(self.phase as u8);
        bytes.push(self.subscriptions);
        bytes.push(self.command_seq);
        bytes.extend_from_slice(&self.next_operation.to_le_bytes());
        bytes.push(self.history_retries);
        bytes.push(self.config_step);
        bytes.push(u8::from(self.paired));
        Ok(bytes)
    }
}

impl Whoop5Connector {
    fn advance_configuration(
        &mut self,
        event: &ConnectorEvent,
    ) -> Result<ActionBatch, ConnectorError> {
        match self.config_step {
            0 => {
                self.config_step = 1;
                // Opcode 3 is toggle_realtime_hr. Sent with an empty body it toggles nothing, so
                // the strap never starts packet-40 REALTIME_DATA and the only heart rate the host
                // ever sees arrives in the 60-second historical offload. The explicit enable byte
                // follows opcode 63's on/off convention.
                let command = self.command(3, &[1])?;
                self.write(event, command)
            }
            1 => {
                self.config_step = 2;
                let command = self.command(117, &[])?;
                self.write(event, command)
            }
            2 => {
                self.config_step = 3;
                let command = self.command(118, &[])?;
                self.write(event, command)
            }
            FIRST_FLAG_STEP..=LAST_FLAG_STEP => {
                let index = usize::from(self.config_step - FIRST_FLAG_STEP);
                let (name, value) = FEATURE_FLAGS.get(index).copied().ok_or_else(|| {
                    ConnectorError::InvalidWire("feature flag step out of range".to_owned())
                })?;
                self.config_step += 1;
                let seq = self.take_command_seq();
                let command =
                    whoop_protocol::set_config(seq, name, value).map_err(protocol_error)?;
                self.write(event, command)
            }
            // Open the config key exchange. Revision 1 is the value the strap accepts; anything
            // else, including the empty body this connector used to send, is refused.
            #[cfg(feature = "ecg-probe")]
            KEY_OPEN_STEP => {
                self.config_step += 1;
                let command = self.command(117, &[CONFIG_KEY_REVISION])?;
                self.probe_write(event, command, "117 open key exchange (revision 1)")
            }
            // Walk the key table. Each call answers with one key's name.
            #[cfg(feature = "ecg-probe")]
            KEY_WALK_FIRST..=KEY_WALK_LAST => {
                let index = self.config_step - KEY_WALK_FIRST;
                self.config_step += 1;
                let command = self.command(118, &[CONFIG_KEY_REVISION])?;
                self.probe_write(event, command, &format!("118 next key #{index}"))
            }
            #[cfg(feature = "ecg-probe")]
            ECG_FLAG_STEP => {
                self.config_step += 1;
                let seq = self.take_command_seq();
                let command = whoop_protocol::set_config(seq, "enable_raw_data_w_ecg", b'2')
                    .map_err(protocol_error)?;
                self.probe_write(event, command, "SET_CONFIG enable_raw_data_w_ecg=2")
            }
            #[cfg(feature = "ecg-probe")]
            ECG_START_STEP => {
                self.config_step += 1;
                // Opcode 63 with a [0x01] revision byte is the real raw-AFE trigger, cracked on a
                // live MG. START_RAW_DATA (81) is accepted but streams nothing; enable_raw_data_w_ecg
                // is not a config key on this firmware. See docs/protocol/whoop.md.
                let command = self.command(whoop_protocol::START_AFE_RAW, &[1])?;
                self.probe_write(event, command, "START_AFE_RAW(63) [0x01]")
            }
            _ => {
                self.phase = Phase::Streaming;
                self.actions(
                    event,
                    vec![
                        ActionBody::DeclareCapabilities { streams: streams() },
                        ActionBody::EmitDiagnostic {
                            level: DiagnosticLevel::Info,
                            code: "whoop5-r22-unverified".to_owned(),
                            message: "R22 flags sent; deep-stream availability remains unverified"
                                .to_owned(),
                        },
                        ActionBody::SetTimer {
                            token: IDLE_TIMER,
                            delay_ms: IDLE_DELAY_MS,
                        },
                    ],
                )
            }
        }
    }

    fn notification(
        &mut self,
        event: &ConnectorEvent,
        characteristic_id: &str,
        bytes: &[u8],
    ) -> Result<ActionBatch, ConnectorError> {
        if characteristic_id == STANDARD_HR_ID {
            let wall = event.wall_time_ms.ok_or_else(|| {
                ConnectorError::InvalidWire("standard heart-rate event has no wall time".to_owned())
            })?;
            self.last_live_ms = Some(wall);
            return self.emit_or_diagnose(event, decode_standard_heart_rate(bytes, wall), false);
        }
        let Some(deframer) = self.deframers.get_mut(characteristic_id) else {
            return self.diagnostic(
                event,
                DiagnosticLevel::Warning,
                "whoop5-channel",
                &format!("notification on unknown characteristic {characteristic_id}"),
            );
        };
        // One notification can carry a fragment, a whole frame, or several packed together.
        let frames = deframer.push(bytes);
        let mut actions = Vec::new();
        for frame in frames {
            let batch = match frame {
                // Console output is not confined to the console characteristic — a live strap sends
                // it on the data channel too. Route by what the frame says it is.
                Ok(payload) if payload.first() == Some(&CONSOLE_LOGS_PACKET) => {
                    self.console_text(event, &payload)?
                }
                Ok(payload) => self.frame(event, &payload)?,
                Err(error) => self.diagnostic(
                    event,
                    DiagnosticLevel::Warning,
                    "whoop5-frame",
                    &format!("malformed WHOOP 5.0/MG frame: {error:?}"),
                )?,
            };
            actions.extend(batch.actions);
        }
        Ok(ActionBatch { actions })
    }

    /// Surface what a command response actually answered: battery reaches the pipeline as a
    /// sample, identity and the banked-history window as diagnostics.
    fn response_body(
        &mut self,
        event: &ConnectorEvent,
        payload: &[u8],
    ) -> Result<ActionBatch, ConnectorError> {
        let time_ms = event.wall_time_ms;
        #[cfg(feature = "ecg-probe")]
        let announced = {
            let status = match decode_control(Generation::Gen5, payload) {
                Ok(Some(Control::Response {
                    to_opcode, result, ..
                })) => format!("opcode {to_opcode} → {result:?}"),
                _ => "unparsed".to_owned(),
            };
            self.probe_note(
                event,
                "whoop5-probe-response",
                &format!("{status} · {}", probe_hex(payload)),
            )?
        };
        #[cfg(not(feature = "ecg-probe"))]
        let announced = empty();
        let mut batch = announced;
        let decoded = match decode_response(Generation::Gen5, payload).map_err(protocol_error)? {
            CommandResponse::Battery { deci_percent } if deci_percent <= 1000 => {
                let Some(time_ms) = time_ms else {
                    return Ok(empty());
                };
                self.actions(
                    event,
                    vec![ActionBody::EmitSamples {
                        batch_id: BatchId(self.next_operation),
                        samples: vec![WireSample {
                            stream: "battery-soc".to_owned(),
                            value_microunits: i64::from(deci_percent) * 100_000,
                            device_time_ms: Some(time_ms),
                            sequence: 0,
                            unit: "percent".to_owned(),
                        }],
                    }],
                )
            }
            CommandResponse::Hello {
                device_name,
                firmware,
            } => self.diagnostic(
                event,
                DiagnosticLevel::Info,
                "whoop5-identity",
                &format!("strap {device_name}, firmware {firmware:?}"),
            ),
            CommandResponse::DataRange { oldest, newest } => self.diagnostic(
                event,
                DiagnosticLevel::Info,
                "whoop5-data-range",
                &format!("banked history spans {oldest:?}..{newest:?}"),
            ),
            _ => Ok(empty()),
        }?;
        batch.actions.extend(decoded.actions);
        Ok(batch)
    }

    /// Console frames carry firmware log text behind a ten-byte record header. They are surfaced as
    /// diagnostics and never as samples: nothing on this channel is a measurement.
    fn console_text(
        &mut self,
        event: &ConnectorEvent,
        payload: &[u8],
    ) -> Result<ActionBatch, ConnectorError> {
        const HEADER: usize = 10;
        const MAX_TEXT: usize = 2048;
        let mut text = String::new();
        for &byte in payload.get(HEADER..).unwrap_or(&[]) {
            if byte == b'\n' {
                text.push('\n');
            } else if (32..=126).contains(&byte) {
                text.push(byte as char);
            }
            if text.len() >= MAX_TEXT {
                break;
            }
        }
        if text.is_empty() {
            return Ok(empty());
        }
        #[cfg(feature = "ecg-probe")]
        return self.probe_note(event, "whoop5-console", &text);
        #[cfg(not(feature = "ecg-probe"))]
        self.diagnostic(event, DiagnosticLevel::Info, "whoop5-console", &text)
    }

    fn frame(
        &mut self,
        event: &ConnectorEvent,
        payload: &[u8],
    ) -> Result<ActionBatch, ConnectorError> {
        match decode_control(Generation::Gen5, payload).map_err(protocol_error)? {
            Some(control @ Control::Response { .. }) => {
                // The control gate reads the status; the body carries the answer itself.
                let mut batch = self.control(event, control)?;
                batch
                    .actions
                    .extend(self.response_body(event, payload)?.actions);
                Ok(batch)
            }
            Some(control) => self.control(event, control),
            None => {
                let refresh_deadline = self.phase == Phase::Historical;
                if !refresh_deadline {
                    self.last_live_ms = event.wall_time_ms;
                }
                match decode_payload(payload) {
                    Ok(samples) => self.emit_or_diagnose(event, Ok(samples), refresh_deadline),
                    // A packet type with no decoder is the edge of the map, not a malformed frame.
                    // Name it and say how big it was, so a frontier type shows up as itself rather
                    // than as an anonymous decode failure.
                    Err(decode::DecodeError::UnknownPacket(kind)) => {
                        self.unmapped_packet(event, kind, payload)
                    }
                    Err(error) => self.emit_or_diagnose(event, Err(error), refresh_deadline),
                }
            }
        }
    }

    /// Report a frame whose packet type has no decoder. Always names the type and its length; a
    /// probe build additionally reports the leading bytes as hex, because reading them is the only
    /// way anyone decodes a type nobody has decoded.
    fn unmapped_packet(
        &mut self,
        event: &ConnectorEvent,
        kind: u8,
        payload: &[u8],
    ) -> Result<ActionBatch, ConnectorError> {
        let named = whoop_protocol::PacketKind::from_u8(kind);
        #[cfg_attr(not(feature = "ecg-probe"), allow(unused_mut))]
        let mut message = format!(
            "unmapped packet {kind} ({}), {} bytes",
            named.name(),
            payload.len()
        );
        #[cfg(feature = "ecg-probe")]
        {
            message.push_str(" · ");
            message.push_str(&probe_hex(payload));
        }
        self.diagnostic(
            event,
            DiagnosticLevel::Info,
            "whoop5-unmapped-packet",
            &message,
        )
    }

    fn control(
        &mut self,
        event: &ConnectorEvent,
        control: Control,
    ) -> Result<ActionBatch, ConnectorError> {
        match control {
            Control::Response {
                to_opcode: 34,
                result: ControlResult::Ok,
                ..
            } => {
                let seq = self.take_command_seq();
                let command = request_history(Generation::Gen5, seq).map_err(protocol_error)?;
                self.write_with_timeout(event, command)
            }
            Control::Response {
                to_opcode: 22,
                result: ControlResult::Ok | ControlResult::Pending,
                ..
            }
            | Control::MetadataStart { .. } => {
                self.phase = Phase::Historical;
                self.actions(
                    event,
                    vec![ActionBody::SetTimer {
                        token: RESPONSE_TIMER,
                        delay_ms: 5_000,
                    }],
                )
            }
            Control::MetadataEnd { cursor, .. } => {
                let seq = self.take_command_seq();
                let command = history_ack(Generation::Gen5, seq, cursor).map_err(protocol_error)?;
                self.write(event, command)
            }
            Control::MetadataComplete { .. } => {
                self.phase = Phase::Streaming;
                self.actions(
                    event,
                    vec![
                        ActionBody::CancelTimer {
                            token: RESPONSE_TIMER,
                        },
                        ActionBody::SetTimer {
                            token: IDLE_TIMER,
                            delay_ms: IDLE_DELAY_MS,
                        },
                    ],
                )
            }
            Control::Response { .. } | Control::MetadataUnknown { .. } => Ok(empty()),
        }
    }

    fn emit_or_diagnose(
        &mut self,
        event: &ConnectorEvent,
        decoded: Result<Vec<WireSample>, decode::DecodeError>,
        refresh_deadline: bool,
    ) -> Result<ActionBatch, ConnectorError> {
        let mut bodies = Vec::new();
        if refresh_deadline {
            bodies.push(ActionBody::SetTimer {
                token: RESPONSE_TIMER,
                delay_ms: 5_000,
            });
        }
        match decoded {
            Ok(samples) if samples.is_empty() => {}
            Ok(samples) => bodies.extend(samples.chunks(MAX_SAMPLES_PER_ACTION).enumerate().map(
                |(index, chunk)| ActionBody::EmitSamples {
                    batch_id: BatchId(self.next_operation.saturating_add(index as u64)),
                    samples: chunk.to_vec(),
                },
            )),
            Err(error) => bodies.push(ActionBody::EmitDiagnostic {
                level: DiagnosticLevel::Warning,
                code: "whoop5-decode".to_owned(),
                message: format!("WHOOP 5.0/MG payload rejected: {error:?}"),
            }),
        }
        if bodies.is_empty() {
            Ok(empty())
        } else {
            self.actions(event, bodies)
        }
    }

    /// A probe diagnostic, if the session can still afford one. Returns an empty batch once the
    /// budget is spent, so a chatty strap cannot exhaust the host's operation allowance and kill
    /// the session before the probe finishes.
    #[cfg(feature = "ecg-probe")]
    fn probe_note(
        &mut self,
        event: &ConnectorEvent,
        code: &str,
        message: &str,
    ) -> Result<ActionBatch, ConnectorError> {
        if self.probe_budget == 0 {
            return Ok(empty());
        }
        self.probe_budget -= 1;
        self.diagnostic(event, DiagnosticLevel::Info, code, message)
    }

    /// A probe write, announced. Without this the journal cannot distinguish "the strap ignored
    /// the command" from "the command was never sent", and those need completely different fixes.
    #[cfg(feature = "ecg-probe")]
    fn probe_write(
        &mut self,
        event: &ConnectorEvent,
        bytes: Vec<u8>,
        what: &str,
    ) -> Result<ActionBatch, ConnectorError> {
        let mut batch = self.probe_note(
            event,
            "whoop5-probe-step",
            &format!("sending {what} · {}", probe_hex(&bytes)),
        )?;
        batch.actions.extend(self.write(event, bytes)?.actions);
        Ok(batch)
    }

    fn write(
        &mut self,
        event: &ConnectorEvent,
        bytes: Vec<u8>,
    ) -> Result<ActionBatch, ConnectorError> {
        self.actions(
            event,
            vec![ActionBody::Write {
                characteristic_id: COMMAND_ID.to_owned(),
                bytes,
                confirmed: true,
            }],
        )
    }

    fn write_with_timeout(
        &mut self,
        event: &ConnectorEvent,
        bytes: Vec<u8>,
    ) -> Result<ActionBatch, ConnectorError> {
        self.actions(
            event,
            vec![
                ActionBody::Write {
                    characteristic_id: COMMAND_ID.to_owned(),
                    bytes,
                    confirmed: true,
                },
                ActionBody::SetTimer {
                    token: RESPONSE_TIMER,
                    delay_ms: 5_000,
                },
            ],
        )
    }

    fn diagnostic(
        &mut self,
        event: &ConnectorEvent,
        level: DiagnosticLevel,
        code: &str,
        message: &str,
    ) -> Result<ActionBatch, ConnectorError> {
        self.actions(
            event,
            vec![ActionBody::EmitDiagnostic {
                level,
                code: code.to_owned(),
                message: message.to_owned(),
            }],
        )
    }

    fn command(&mut self, opcode: u8, body: &[u8]) -> Result<Vec<u8>, ConnectorError> {
        let seq = self.take_command_seq();
        build_command(Generation::Gen5, seq, opcode, body).map_err(protocol_error)
    }

    /// True when realtime notifications are still arriving, which defers historical offload.
    fn live_is_active(&self, now_ms: Option<i64>) -> bool {
        match (now_ms, self.last_live_ms) {
            (Some(now), Some(last)) => now.saturating_sub(last) < LIVE_ACTIVE_MS,
            _ => false,
        }
    }

    fn take_command_seq(&mut self) -> u8 {
        let value = self.command_seq;
        self.command_seq = self.command_seq.wrapping_add(1);
        value
    }

    fn actions(
        &mut self,
        event: &ConnectorEvent,
        bodies: Vec<ActionBody>,
    ) -> Result<ActionBatch, ConnectorError> {
        let mut builder = ActionBuilder::for_event(event);
        for body in bodies {
            let operation = OperationId(self.next_operation);
            self.next_operation = self.next_operation.saturating_add(1);
            builder = builder.push(operation, TimerToken(operation.0), body)?;
        }
        builder.finish()
    }

    fn restore(&mut self, bytes: &[u8]) -> Result<(), ConnectorError> {
        if bytes.len() != SNAPSHOT_LEN || bytes.get(..3) != Some(b"W5\x01") {
            return Err(ConnectorError::InvalidWire(
                "WHOOP 5.0/MG state snapshot is malformed".to_owned(),
            ));
        }
        self.phase = phase(bytes[3])?;
        self.subscriptions = bytes[4];
        self.command_seq = bytes[5];
        let mut operation = [0u8; 8];
        operation.copy_from_slice(&bytes[6..14]);
        self.next_operation = u64::from_le_bytes(operation).max(1);
        self.history_retries = bytes[14];
        self.config_step = bytes[15];
        self.paired = bytes[16] != 0;
        Ok(())
    }
}

fn empty() -> ActionBatch {
    ActionBatch {
        actions: Vec::new(),
    }
}

fn is_gen5(service_uuids: &[String], name: Option<&str>) -> bool {
    has_uuid(service_uuids, GEN5_SERVICE)
        && name.is_some_and(|value| {
            value == "WHOOP" || value.starts_with("WHOOP 5.0") || value.starts_with("WHOOP MG")
        })
}

fn has_uuid(values: &[String], expected: &str) -> bool {
    values
        .iter()
        .any(|value| value.eq_ignore_ascii_case(expected))
}

fn subscription_bit(id: &str) -> Option<u8> {
    match id {
        STANDARD_HR_ID => Some(1),
        COMMAND_RESPONSE_ID => Some(2),
        EVENTS_ID => Some(4),
        DATA_ID => Some(8),
        CONSOLE_ID => Some(16),
        _ => None,
    }
}

fn phase(value: u8) -> Result<Phase, ConnectorError> {
    match value {
        0 => Ok(Phase::Idle),
        1 => Ok(Phase::Scanning),
        2 => Ok(Phase::Connecting),
        3 => Ok(Phase::Pairing),
        4 => Ok(Phase::Discovering),
        5 => Ok(Phase::Subscribing),
        6 => Ok(Phase::Configuring),
        7 => Ok(Phase::Streaming),
        8 => Ok(Phase::Historical),
        9 => Ok(Phase::Suspended),
        10 => Ok(Phase::Disconnected),
        _ => Err(ConnectorError::InvalidWire(
            "WHOOP 5.0/MG state phase is unknown".to_owned(),
        )),
    }
}

fn protocol_error(error: whoop_protocol::ProtocolError) -> ConnectorError {
    ConnectorError::InvalidWire(format!("WHOOP protocol error: {error:?}"))
}

fn streams() -> Vec<String> {
    [
        "heart-rate",
        "rr-interval",
        "gravity",
        "skin-temp",
        "spo2-percent",
        "step-count",
        "activity-class",
        "sleep-state-raw",
        "signal-quality",
        "ppg",
        "imu",
        "gyro",
        "optical-raw",
        "battery-soc",
        "wrist-state",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

export_connector!(Whoop5Connector);

artifact_metadata! {
    pub fn metadata() {
        manifest: manifest()?,
        abi: abi_descriptor(),
        fixtures: fixture_set()?,
    }
}

fn manifest() -> Result<Manifest, ConnectorError> {
    let transport = vec![
        TransportCapability::Scan,
        TransportCapability::Connect,
        TransportCapability::Pair,
        TransportCapability::Discover,
        TransportCapability::Subscribe,
        TransportCapability::Read,
        TransportCapability::Write,
    ];
    Ok(Manifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        connector_id: ConnectorId::new(CONNECTOR_ID)?,
        version: CONNECTOR_VERSION.to_owned(),
        display_name: "WHOOP 5.0 / MG".to_owned(),
        description: "Local WHOOP 5.0/MG connector; deep availability remains unverified"
            .to_owned(),
        publisher_key_id: "maverick-whoop-live-test".to_owned(),
        abi: AbiRange {
            major: 1,
            min_minor: 0,
            max_minor: 0,
        },
        core: CoreRange {
            min_version: "0.1.0".to_owned(),
            max_version: None,
        },
        state_schema: 1,
        artifact_limits_profile: LimitsProfileId::new("mobile-v1")?,
        device_families: vec![DeviceFamily {
            id: "whoop5-mg".to_owned(),
            name_prefixes: vec![
                "WHOOP 5.0".to_owned(),
                "WHOOP MG".to_owned(),
                "WHOOP".to_owned(),
            ],
            service_uuids: vec![GEN5_SERVICE.to_owned()],
            manufacturer_id: None,
            manufacturer_mask: Vec::new(),
            manufacturer_value: Vec::new(),
        }],
        services: services(),
        capabilities: streams()
            .into_iter()
            .map(|stream| CapabilityDecl {
                stream,
                transport: transport.clone(),
            })
            .collect(),
        permissions: vec![Permission::Ble],
        entrypoints: Entrypoints::default(),
        fixture_set_hash: [0; 32],
        update: UpdatePolicy {
            channel: "stable".to_owned(),
            downgrade: DowngradePolicy::Reject,
        },
    })
}

fn services() -> Vec<ServiceDecl> {
    vec![
        ServiceDecl {
            id: "whoop5-custom".to_owned(),
            uuid: GEN5_SERVICE.to_owned(),
            characteristics: vec![
                characteristic(
                    COMMAND_ID,
                    "fd4b0002-cce1-4033-93ce-002d5875f58a",
                    vec![CharacteristicProperty::Write],
                    true,
                ),
                characteristic(
                    COMMAND_RESPONSE_ID,
                    "fd4b0003-cce1-4033-93ce-002d5875f58a",
                    vec![CharacteristicProperty::Notify],
                    false,
                ),
                characteristic(
                    EVENTS_ID,
                    "fd4b0004-cce1-4033-93ce-002d5875f58a",
                    vec![CharacteristicProperty::Notify],
                    false,
                ),
                characteristic(
                    DATA_ID,
                    "fd4b0005-cce1-4033-93ce-002d5875f58a",
                    vec![CharacteristicProperty::Notify],
                    false,
                ),
                characteristic(
                    CONSOLE_ID,
                    "fd4b0007-cce1-4033-93ce-002d5875f58a",
                    vec![CharacteristicProperty::Notify],
                    false,
                ),
            ],
        },
        ServiceDecl {
            id: "heart-rate-service".to_owned(),
            uuid: "180d".to_owned(),
            characteristics: vec![characteristic(
                STANDARD_HR_ID,
                "2a37",
                vec![CharacteristicProperty::Notify],
                false,
            )],
        },
        ServiceDecl {
            id: "battery-service".to_owned(),
            uuid: "180f".to_owned(),
            characteristics: vec![characteristic(
                "battery-level",
                "2a19",
                vec![CharacteristicProperty::Read],
                false,
            )],
        },
        ServiceDecl {
            id: "device-information".to_owned(),
            uuid: "180a".to_owned(),
            characteristics: vec![characteristic(
                "model-number",
                "2a24",
                vec![CharacteristicProperty::Read],
                false,
            )],
        },
    ]
}

fn characteristic(
    id: &str,
    uuid: &str,
    properties: Vec<CharacteristicProperty>,
    confirmed_write_required: bool,
) -> CharacteristicDecl {
    CharacteristicDecl {
        id: id.to_owned(),
        uuid: uuid.to_owned(),
        properties,
        sensitive: false,
        confirmed_write_required,
    }
}

fn abi_descriptor() -> AbiDescriptor {
    AbiDescriptor {
        schema: ABI_SCHEMA.to_owned(),
        version: AbiVersion { major: 1, minor: 0 },
        schema_hash: ABI_V1_SCHEMA_HASH,
        required_exports: [
            "memory",
            "mav_abi_version",
            "mav_alloc",
            "mav_dealloc",
            "mav_init",
            "mav_handle",
            "mav_snapshot",
        ]
        .map(str::to_owned)
        .to_vec(),
        required_imports: Vec::new(),
        wasm_features: vec![
            WasmFeature::MutableGlobals,
            WasmFeature::SignExtension,
            WasmFeature::BulkMemory,
        ],
        sdk_version: "0.1.1".to_owned(),
    }
}

fn fixture_set() -> Result<FixtureSet, ConnectorError> {
    let event = ConnectorEvent {
        connector_id: ConnectorId::new(CONNECTOR_ID)?,
        session_id: SessionId(1),
        sequence: EventSequence(1),
        cancellation_generation: CancellationGeneration(0),
        wall_time_ms: None,
        body: EventBody::Activate,
    };
    let expected = ActionBatch {
        actions: vec![ConnectorAction {
            connector_id: ConnectorId::new(CONNECTOR_ID)?,
            session_id: SessionId(1),
            caused_by: EventSequence(1),
            cancellation_generation: CancellationGeneration(0),
            operation_id: OperationId(1),
            deadline_token: TimerToken(1),
            body: ActionBody::StartScan {
                service_uuids: vec![GEN5_SERVICE.to_owned(), "180d".to_owned()],
                manufacturer_ids: Vec::new(),
            },
        }],
    };
    let mut cases = vec![FixtureCase {
        name: "activate-gen5-scan".to_owned(),
        initial_state: Vec::new(),
        events: vec![event],
        expected: vec![expected],
        expected_state_hash: [
            0xf2, 0x28, 0xd8, 0xa5, 0x5f, 0x9b, 0xc3, 0xce, 0x1e, 0x6c, 0x26, 0x3c, 0xf2, 0x37,
            0xc2, 0xfd, 0x46, 0x0c, 0x2b, 0xa2, 0xdb, 0x88, 0x36, 0x44, 0x73, 0xbf, 0x51, 0x72,
            0x3b, 0x9b, 0xd1, 0xac,
        ],
        max_fuel: 1_000_000,
        expected_samples: None,
        expected_diagnostics: None,
    }];
    cases.extend(record_fixtures()?);
    cases.extend(stream_fixtures()?);
    cases.extend(parity_fixtures()?);
    Ok(FixtureSet {
        schema: FIXTURES_SCHEMA.to_owned(),
        cases,
    })
}

fn parity_fixtures() -> Result<Vec<FixtureCase>, ConnectorError> {
    let history_end =
        fixture_hex("aa011c00010023d1319102b949596a705d3b000000fdba010010000000000000f269faec")?;
    let record = fixture_hex(include_str!(
        "../../../crates/whoop-protocol/tests/fixtures/whoop_rs_gen5_v18.hex"
    ))?;
    let (head, tail) = record.split_at(20);
    let mut packed = record.clone();
    packed.extend_from_slice(&record);
    Ok(vec![
        native_parity_fixture(
            "frame-split-across-notifications",
            streaming_fixture_state(),
            vec![
                fixture_event(
                    1,
                    EventBody::Notification {
                        characteristic_id: DATA_ID.to_owned(),
                        bytes: head.to_vec(),
                    },
                )?,
                fixture_event(
                    2,
                    EventBody::Notification {
                        characteristic_id: DATA_ID.to_owned(),
                        bytes: tail.to_vec(),
                    },
                )?,
            ],
        )?,
        native_parity_fixture(
            "frames-packed-in-one-notification",
            streaming_fixture_state(),
            vec![fixture_event(
                1,
                EventBody::Notification {
                    characteristic_id: DATA_ID.to_owned(),
                    bytes: packed,
                },
            )?],
        )?,
        native_parity_fixture(
            "history-cursor-retry",
            streaming_fixture_state(),
            vec![
                fixture_event(1, EventBody::TimerFired { token: IDLE_TIMER })?,
                fixture_event(
                    2,
                    EventBody::Notification {
                        characteristic_id: COMMAND_RESPONSE_ID.to_owned(),
                        bytes: fixture_gen5_frame(&[0x24, 1, 34, 0, 1])?,
                    },
                )?,
                fixture_event(
                    3,
                    EventBody::Notification {
                        characteristic_id: DATA_ID.to_owned(),
                        bytes: history_end,
                    },
                )?,
                fixture_event(
                    4,
                    EventBody::TimerFired {
                        token: RESPONSE_TIMER,
                    },
                )?,
            ],
        )?,
        native_parity_fixture(
            "state-restart",
            streaming_fixture_state(),
            vec![fixture_event(1, EventBody::Resume)?],
        )?,
        native_parity_fixture(
            "malformed-frame",
            streaming_fixture_state(),
            vec![fixture_event(
                1,
                EventBody::Notification {
                    characteristic_id: DATA_ID.to_owned(),
                    bytes: corrupt_gen5_frame()?,
                },
            )?],
        )?,
    ])
}

fn native_parity_fixture(
    name: &str,
    initial_state: Vec<u8>,
    events: Vec<ConnectorEvent>,
) -> Result<FixtureCase, ConnectorError> {
    let first = events.first().ok_or_else(|| {
        ConnectorError::InvalidWire("parity fixture must contain an event".to_owned())
    })?;
    let mut driver = TestDriver::new(Whoop5Connector::default());
    let mut restore = first.clone();
    restore.body = EventBody::RestoreState {
        bytes: initial_state.clone(),
    };
    if !driver.init(restore)?.actions.is_empty() {
        return Err(ConnectorError::InvalidWire(
            "state restore emitted parity actions".to_owned(),
        ));
    }
    let mut expected = Vec::with_capacity(events.len());
    for event in &events {
        expected.push(driver.drive(event.clone())?);
    }
    let expected_state_hash = Sha256::digest(driver.snapshot()?).into();
    Ok(FixtureCase {
        name: name.to_owned(),
        initial_state,
        events,
        expected,
        expected_state_hash,
        max_fuel: 1_000_000,
        expected_samples: None,
        expected_diagnostics: None,
    })
}

fn fixture_event(sequence: u64, body: EventBody) -> Result<ConnectorEvent, ConnectorError> {
    Ok(ConnectorEvent {
        connector_id: ConnectorId::new(CONNECTOR_ID)?,
        session_id: SessionId(1),
        sequence: EventSequence(sequence),
        cancellation_generation: CancellationGeneration(0),
        wall_time_ms: Some(1_780_000_000_000),
        body,
    })
}

fn record_fixtures() -> Result<Vec<FixtureCase>, ConnectorError> {
    let v18 = fixture_hex(include_str!(
        "../../../crates/whoop-protocol/tests/fixtures/whoop_rs_gen5_v18.hex"
    ))?;
    let v26 = fixture_hex(include_str!(
        "../../../crates/whoop-protocol/tests/fixtures/whoop_rs_gen5_v26.hex"
    ))?;
    let time_v18 = 1_780_916_150_000;
    let v18_samples = vec![
        wire_sample("heart-rate", 102_000_000, time_v18, 0, "beats-per-minute"),
        wire_sample("rr-interval", 602_000_000, time_v18, 0, "milliseconds"),
        wire_sample("rr-interval", 613_000_000, time_v18, 1, "milliseconds"),
        wire_sample("gravity", -725_173, time_v18, 0, "milli-g"),
        wire_sample("gravity", 494_417, time_v18, 1, "milli-g"),
        wire_sample("gravity", 496_855, time_v18, 2, "milli-g"),
        wire_sample("skin-temp", 30_570_000, time_v18, 0, "degrees-celsius"),
        wire_sample("step-count", 50_000_000, time_v18, 0, "count"),
        wire_sample("activity-class", 0, time_v18, 0, "code"),
        wire_sample("sleep-state-raw", 0, time_v18, 0, "code"),
        wire_sample("signal-quality", 255_000_000, time_v18, 0, "percent"),
        // The worn marker the record carries at inner [2]; the real capture has 0x80 there.
        wire_sample("wrist-state", 1_000_000, time_v18, 0, "boolean"),
    ];
    let ppg_values = [
        292, 306, 463, 553, 9, -1550, -1952, -1503, -1082, -791, -343, -346, -352, -313, -162,
        -133, 100, 102, 252, 344, 327, 460, 291, -902,
    ];
    let mut v26_samples: Vec<WireSample> = ppg_values
        .into_iter()
        .enumerate()
        .map(|(sequence, value)| {
            wire_sample(
                "ppg",
                i64::from(value) * 1_000_000,
                1_783_955_687_000,
                sequence as u32,
                "counts",
            )
        })
        .collect();
    v26_samples.push(wire_sample(
        "wrist-state",
        1_000_000,
        1_783_955_687_000,
        0,
        "boolean",
    ));

    let mut v20_payload = vec![0u8; 2_027];
    v20_payload[0] = 47;
    v20_payload[1] = 20;
    let body = &mut v20_payload[3..];
    body[4..8].copy_from_slice(&1_780_000_000u32.to_le_bytes());
    body[17..19].copy_from_slice(&1400u16.to_le_bytes());
    body[20..22].copy_from_slice(&2800u16.to_le_bytes());
    body[36..40].copy_from_slice(&12345u32.to_le_bytes());
    body[40..44].copy_from_slice(&0x000f_fffbu32.to_le_bytes());
    let mut v20_samples = Vec::with_capacity(150);
    for sequence in 0..150 {
        let value = match sequence {
            0 => 12_345_000_000,
            1 => -5_000_000,
            _ => 0,
        };
        v20_samples.push(wire_sample(
            "optical-raw",
            value,
            1_780_000_000_000,
            sequence,
            "counts",
        ));
    }

    let mut v21_payload = vec![0u8; 1_232];
    v21_payload[0] = 47;
    v21_payload[1] = 21;
    let body = &mut v21_payload[3..];
    body[4..8].copy_from_slice(&1_780_000_000u32.to_le_bytes());
    body[13..15].copy_from_slice(&100u16.to_le_bytes());
    body[619..621].copy_from_slice(&100u16.to_le_bytes());
    body[17..19].copy_from_slice(&4096i16.to_le_bytes());
    body[629..631].copy_from_slice(&250i16.to_le_bytes());
    let mut v21_samples = Vec::with_capacity(600);
    for (stream, first, unit) in [
        ("imu", 4_096_000_000, "milli-g"),
        ("gyro", 250_000_000, "milli-degrees-per-second"),
    ] {
        for sequence in 0..300 {
            v21_samples.push(wire_sample(
                stream,
                if sequence == 0 { first } else { 0 },
                1_780_000_000_000,
                sequence,
                unit,
            ));
        }
    }

    Ok(vec![
        notification_fixture("real-gen5-v18", DATA_ID, v18, v18_samples)?,
        notification_fixture("real-gen5-v26", DATA_ID, v26, v26_samples)?,
        notification_fixture(
            "synthetic-gen5-v20",
            DATA_ID,
            fixture_gen5_frame(&v20_payload)?,
            v20_samples,
        )?,
        notification_fixture(
            "synthetic-gen5-v21",
            DATA_ID,
            fixture_gen5_frame(&v21_payload)?,
            v21_samples,
        )?,
    ])
}

fn stream_fixtures() -> Result<Vec<FixtureCase>, ConnectorError> {
    let time = 1_780_000_000_000;
    let mut realtime = vec![0u8; 14];
    realtime[0] = 40;
    realtime[2..6].copy_from_slice(&1_780_000_000u32.to_le_bytes());
    realtime[8] = 64;
    realtime[9] = 2;
    realtime[10..12].copy_from_slice(&800u16.to_le_bytes());
    realtime[12..14].copy_from_slice(&810u16.to_le_bytes());
    let mut battery = vec![0u8; 24];
    battery[0] = 48;
    battery[2] = 3;
    battery[4..8].copy_from_slice(&1_780_000_000u32.to_le_bytes());
    battery[13..15].copy_from_slice(&812u16.to_le_bytes());
    let mut wrist = battery.clone();
    wrist[2] = 9;

    Ok(vec![
        notification_fixture(
            "standard-heart-rate",
            STANDARD_HR_ID,
            vec![0x10, 64, 0x33, 0x03],
            vec![
                wire_sample("heart-rate", 64_000_000, time, 0, "beats-per-minute"),
                wire_sample("rr-interval", 800_000_000, time, 0, "milliseconds"),
            ],
        )?,
        notification_fixture(
            "custom-realtime",
            DATA_ID,
            fixture_gen5_frame(&realtime)?,
            vec![
                wire_sample("heart-rate", 64_000_000, time, 0, "beats-per-minute"),
                wire_sample("rr-interval", 800_000_000, time, 0, "milliseconds"),
                wire_sample("rr-interval", 810_000_000, time, 1, "milliseconds"),
            ],
        )?,
        notification_fixture(
            "battery-event",
            EVENTS_ID,
            fixture_gen5_frame(&battery)?,
            vec![wire_sample("battery-soc", 81_200_000, time, 0, "percent")],
        )?,
        notification_fixture(
            "wrist-event",
            EVENTS_ID,
            fixture_gen5_frame(&wrist)?,
            vec![wire_sample("wrist-state", 1_000_000, time, 0, "boolean")],
        )?,
    ])
}

fn notification_fixture(
    name: &str,
    characteristic_id: &str,
    bytes: Vec<u8>,
    samples: Vec<WireSample>,
) -> Result<FixtureCase, ConnectorError> {
    let event = ConnectorEvent {
        connector_id: ConnectorId::new(CONNECTOR_ID)?,
        session_id: SessionId(1),
        sequence: EventSequence(1),
        cancellation_generation: CancellationGeneration(0),
        wall_time_ms: Some(1_780_000_000_000),
        body: EventBody::Notification {
            characteristic_id: characteristic_id.to_owned(),
            bytes,
        },
    };
    let mut actions = Vec::new();
    for (index, chunk) in samples.chunks(MAX_SAMPLES_PER_ACTION).enumerate() {
        let id = u64::try_from(index + 1)
            .map_err(|_| ConnectorError::InvalidWire("fixture action index overflow".to_owned()))?;
        actions.push(ConnectorAction {
            connector_id: ConnectorId::new(CONNECTOR_ID)?,
            session_id: SessionId(1),
            caused_by: EventSequence(1),
            cancellation_generation: CancellationGeneration(0),
            operation_id: OperationId(id),
            deadline_token: TimerToken(id),
            body: ActionBody::EmitSamples {
                batch_id: BatchId(id),
                samples: chunk.to_vec(),
            },
        });
    }
    let state_hash = if actions.len() == 1 {
        [
            0x9e, 0xf9, 0xec, 0x4d, 0xee, 0xaa, 0x9b, 0x67, 0x54, 0xa0, 0x5b, 0x4c, 0xe7, 0x92,
            0xdf, 0xc9, 0x60, 0xd0, 0xee, 0x39, 0xf6, 0x58, 0x06, 0x4a, 0xbb, 0xb7, 0x94, 0x57,
            0xf2, 0xb3, 0x96, 0x05,
        ]
    } else {
        [
            0x6b, 0xc2, 0xab, 0x5c, 0xe4, 0x50, 0x8c, 0x4d, 0x84, 0x4d, 0x58, 0x9d, 0x7d, 0x13,
            0x78, 0x78, 0x90, 0x5c, 0x89, 0xb6, 0x62, 0xcf, 0x68, 0x45, 0xdf, 0x92, 0x76, 0x51,
            0xfc, 0x92, 0xfe, 0x4b,
        ]
    };
    let expected_samples = (samples.len() <= MAX_SAMPLES_PER_ACTION).then_some(samples);
    Ok(FixtureCase {
        name: name.to_owned(),
        initial_state: streaming_fixture_state(),
        events: vec![event],
        expected: vec![ActionBatch { actions }],
        expected_state_hash: state_hash,
        max_fuel: 5_000_000,
        expected_samples,
        expected_diagnostics: None,
    })
}

fn streaming_fixture_state() -> Vec<u8> {
    vec![
        0x57,
        0x35,
        1,
        Phase::Streaming as u8,
        0x1f,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        12,
        1,
    ]
}

fn wire_sample(stream: &str, value: i64, time: i64, sequence: u32, unit: &str) -> WireSample {
    WireSample {
        stream: stream.to_owned(),
        value_microunits: value,
        device_time_ms: Some(time),
        sequence,
        unit: unit.to_owned(),
    }
}

fn fixture_hex(value: &str) -> Result<Vec<u8>, ConnectorError> {
    let text = value.trim();
    if !text.len().is_multiple_of(2) {
        return Err(ConnectorError::InvalidWire(
            "fixture hex has odd length".to_owned(),
        ));
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digits = core::str::from_utf8(pair).map_err(|error| {
                ConnectorError::InvalidWire(format!("fixture hex is not UTF-8: {error}"))
            })?;
            u8::from_str_radix(digits, 16).map_err(|error| {
                ConnectorError::InvalidWire(format!("fixture hex is invalid: {error}"))
            })
        })
        .collect()
}

/// Leading bytes of a payload as hex, bounded so a diagnostic stays a bounded action.
#[cfg(feature = "ecg-probe")]
fn probe_hex(payload: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let shown = payload.len().min(PROBE_HEX_BYTES);
    let mut hex = String::with_capacity(shown * 2 + 8);
    for byte in &payload[..shown] {
        hex.push(DIGITS[usize::from(byte >> 4)] as char);
        hex.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    if payload.len() > shown {
        hex.push('\u{2026}');
    }
    hex
}

/// A complete frame whose payload CRC fails. A merely truncated notification is no longer
/// malformed: the deframer holds it until the rest of the frame arrives.
fn corrupt_gen5_frame() -> Result<Vec<u8>, ConnectorError> {
    let mut frame = fixture_gen5_frame(&[0x2f, 18, 0, 0])?;
    if let Some(last) = frame.last_mut() {
        *last ^= 0xff;
    }
    Ok(frame)
}

fn fixture_gen5_frame(payload: &[u8]) -> Result<Vec<u8>, ConnectorError> {
    let padded =
        payload.len().checked_add(3).ok_or_else(|| {
            ConnectorError::InvalidWire("fixture payload is too large".to_owned())
        })? / 4
            * 4;
    let declared = u16::try_from(padded + 4)
        .map_err(|_| ConnectorError::InvalidWire("fixture payload is too large".to_owned()))?;
    let length = declared.to_le_bytes();
    let mut frame = vec![0xaa, 1, length[0], length[1], 0, 1, 0, 0];
    let header_crc = whoop_protocol::crc16_modbus(&frame[..6]).to_le_bytes();
    frame[6..8].copy_from_slice(&header_crc);
    frame.extend_from_slice(payload);
    frame.resize(8 + padded, 0);
    frame.extend_from_slice(&whoop_protocol::crc32(&frame[8..]).to_le_bytes());
    Ok(frame)
}
