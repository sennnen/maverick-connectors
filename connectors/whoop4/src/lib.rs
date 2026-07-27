pub mod decode;

use decode::{decode_payload, decode_standard_heart_rate};
use mav_connector_sdk::abi::*;
use mav_connector_sdk::{
    artifact_metadata, export_connector, ActionBuilder, Connector, ConnectorError, TestDriver,
};
use whoop_protocol::{
    build_command, decode_control, decode_frame, decode_response, get_data_range, history_ack,
    request_history, CommandResponse, Control, Deframer, Generation,
};

pub const CONNECTOR_ID: &str = "dev.maverick.whoop4";
pub const GEN4_SERVICE: &str = "61080001-8d6d-82b8-614a-1c8cb0f8dcc6";
const COMMAND_ID: &str = "command";
const STANDARD_HR_ID: &str = "standard-heart-rate";
const COMMAND_RESPONSE_ID: &str = "command-response";
const EVENTS_ID: &str = "events";
const DATA_ID: &str = "data";
const ALL_SUBSCRIPTIONS: u8 = 0x0f;
const IDLE_TIMER: TimerToken = TimerToken(100);
const RESPONSE_TIMER: TimerToken = TimerToken(101);
const IDLE_DELAY_MS: u64 = 60_000;
/// A live sample newer than this keeps the link reserved for streaming, so the periodic offload
/// deadline is pushed out instead of preempting realtime notifications.
const LIVE_ACTIVE_MS: i64 = 10_000;
const SNAPSHOT_LEN: usize = 15;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
enum Phase {
    #[default]
    Idle = 0,
    Scanning = 1,
    Connecting = 2,
    Discovering = 3,
    Subscribing = 4,
    Configuring = 5,
    Streaming = 6,
    Historical = 7,
    Suspended = 8,
    Disconnected = 9,
}

#[derive(Debug)]
pub struct Whoop4Connector {
    phase: Phase,
    subscriptions: u8,
    command_seq: u8,
    next_operation: u64,
    history_retries: u8,
    /// Wall time of the newest live sample. Session-local; deliberately absent from the snapshot so
    /// the state schema and every frozen fixture hash stay unchanged.
    last_live_ms: Option<i64>,
    /// One reassembly buffer per notify characteristic. Also session-local: a frame cut short by a
    /// dropped link must never be resumed across a reconnect.
    deframers: Deframers,
}

/// Frame reassembly for the three framed notify characteristics. Standard heart rate is a Bluetooth
/// SIG characteristic carrying no WHOOP envelope, so it has none.
#[derive(Debug)]
struct Deframers {
    command_response: Deframer,
    events: Deframer,
    data: Deframer,
}

impl Default for Deframers {
    fn default() -> Self {
        Self {
            command_response: Deframer::new(Generation::Gen4),
            events: Deframer::new(Generation::Gen4),
            data: Deframer::new(Generation::Gen4),
        }
    }
}

impl Deframers {
    fn get_mut(&mut self, characteristic_id: &str) -> Option<&mut Deframer> {
        match characteristic_id {
            COMMAND_RESPONSE_ID => Some(&mut self.command_response),
            EVENTS_ID => Some(&mut self.events),
            DATA_ID => Some(&mut self.data),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.command_response.reset();
        self.events.reset();
        self.data.reset();
    }
}

impl Default for Whoop4Connector {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            subscriptions: 0,
            command_seq: 1,
            next_operation: 1,
            history_retries: 0,
            last_live_ms: None,
            deframers: Deframers::default(),
        }
    }
}

impl Connector for Whoop4Connector {
    fn handle(&mut self, event: ConnectorEvent) -> Result<ActionBatch, ConnectorError> {
        match &event.body {
            EventBody::Init { .. } => Ok(empty()),
            EventBody::Activate | EventBody::Resume => {
                self.phase = Phase::Scanning;
                self.subscriptions = 0;
                self.deframers.reset();
                self.actions(
                    &event,
                    vec![ActionBody::StartScan {
                        service_uuids: vec![GEN4_SERVICE.to_owned(), "180d".to_owned()],
                        manufacturer_ids: Vec::new(),
                    }],
                )
            }
            EventBody::Advertisement {
                address,
                service_uuids,
                name,
                ..
            } if self.phase == Phase::Scanning && is_gen4(service_uuids, name.as_deref()) => {
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
                self.phase = Phase::Discovering;
                self.actions(&event, vec![ActionBody::DiscoverServices])
            }
            EventBody::ServicesDiscovered { service_uuids }
                if has_uuid(service_uuids, GEN4_SERVICE) && has_uuid(service_uuids, "180d") =>
            {
                self.phase = Phase::Subscribing;
                self.actions(
                    &event,
                    [STANDARD_HR_ID, COMMAND_RESPONSE_ID, EVENTS_ID, DATA_ID]
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
                "whoop4-services",
                "required WHOOP 4.0 services were not discovered",
            ),
            EventBody::IdentityRead { field_id, bytes } if field_id == "model-number" => {
                let is_gen4_model = core::str::from_utf8(bytes)
                    .is_ok_and(|model| matches!(model.trim(), "4.0" | "WHOOP 4.0"));
                if is_gen4_model {
                    Ok(empty())
                } else {
                    self.phase = Phase::Disconnected;
                    self.actions(
                        &event,
                        vec![
                            ActionBody::EmitDiagnostic {
                                level: DiagnosticLevel::Error,
                                code: "whoop4-identity".to_owned(),
                                message: "model identity does not name WHOOP 4.0".to_owned(),
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
                    // The 4.0 hello needs a nine-byte client-time argument to answer at all; its
                    // content is ignored and an empty body draws silence.
                    let hello = self.command(35, &[0u8; 9])?;
                    self.actions(
                        &event,
                        vec![ActionBody::Write {
                            characteristic_id: COMMAND_ID.to_owned(),
                            bytes: hello,
                            confirmed: true,
                        }],
                    )
                } else {
                    Ok(empty())
                }
            }
            EventBody::WriteResult {
                characteristic_id, ..
            } if characteristic_id == COMMAND_ID && self.phase == Phase::Configuring => {
                self.phase = Phase::Streaming;
                #[cfg_attr(not(feature = "raw-probe"), allow(unused_mut))]
                let mut actions = vec![
                    ActionBody::DeclareCapabilities { streams: streams() },
                    ActionBody::SetTimer {
                        token: IDLE_TIMER,
                        delay_ms: IDLE_DELAY_MS,
                    },
                ];
                // Opcode 63 `[0x01]` starts the type-43 raw AFE stream on gen4 exactly as it does on
                // gen5 — the revision byte is required. Probe builds only: raw streaming costs
                // battery and bandwidth that a release build has no consumer for.
                #[cfg(feature = "raw-probe")]
                {
                    let start_raw = self.command(whoop_protocol::START_AFE_RAW, &[1])?;
                    actions.push(ActionBody::Write {
                        characteristic_id: COMMAND_ID.to_owned(),
                        bytes: start_raw,
                        confirmed: true,
                    });
                }
                self.actions(&event, actions)
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
                let command = get_data_range(Generation::Gen4, seq).map_err(protocol_error)?;
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
                                code: "whoop4-history-timeout".to_owned(),
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
                let command = request_history(Generation::Gen4, seq).map_err(protocol_error)?;
                self.write_with_timeout(&event, command)
            }
            EventBody::Disconnected { .. } => {
                self.phase = Phase::Disconnected;
                self.subscriptions = 0;
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
        bytes.extend_from_slice(b"W4");
        bytes.push(1);
        bytes.push(self.phase as u8);
        bytes.push(self.subscriptions);
        bytes.push(self.command_seq);
        bytes.extend_from_slice(&self.next_operation.to_le_bytes());
        bytes.push(self.history_retries);
        Ok(bytes)
    }
}

impl Whoop4Connector {
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
                "whoop4-channel",
                &format!("notification on unknown characteristic {characteristic_id}"),
            );
        };
        // One notification can carry a fragment, a whole frame, or several packed together.
        let frames = deframer.push(bytes);
        let mut actions = Vec::new();
        for frame in frames {
            let batch = match frame {
                Ok(payload) => self.frame(event, &payload)?,
                Err(error) => self.diagnostic(
                    event,
                    DiagnosticLevel::Warning,
                    "whoop4-frame",
                    &format!("malformed WHOOP 4.0 frame: {error:?}"),
                )?,
            };
            actions.extend(batch.actions);
        }
        Ok(ActionBatch { actions })
    }

    fn frame(
        &mut self,
        event: &ConnectorEvent,
        payload: &[u8],
    ) -> Result<ActionBatch, ConnectorError> {
        match decode_control(Generation::Gen4, payload).map_err(protocol_error)? {
            Some(control @ Control::Response { .. }) => {
                // 4.0 reports no status, so the body is the only thing a response says.
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
                self.emit_or_diagnose(event, decode_payload(payload), refresh_deadline)
            }
        }
    }

    fn control(
        &mut self,
        event: &ConnectorEvent,
        control: Control,
    ) -> Result<ActionBatch, ConnectorError> {
        match control {
            // 4.0 reports no status, so any reply to GET_DATA_RANGE advances the offload.
            Control::Response { to_opcode: 34, .. } => {
                let seq = self.take_command_seq();
                let command = request_history(Generation::Gen4, seq).map_err(protocol_error)?;
                self.write_with_timeout(event, command)
            }
            Control::Response { to_opcode: 22, .. } | Control::MetadataStart { .. } => {
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
                let command = history_ack(Generation::Gen4, seq, cursor).map_err(protocol_error)?;
                self.actions(
                    event,
                    vec![ActionBody::Write {
                        characteristic_id: COMMAND_ID.to_owned(),
                        bytes: command,
                        confirmed: true,
                    }],
                )
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

    /// Surface what a command response actually answered: battery reaches the pipeline as a
    /// sample, identity, clock, and the banked-history window as diagnostics.
    fn response_body(
        &mut self,
        event: &ConnectorEvent,
        payload: &[u8],
    ) -> Result<ActionBatch, ConnectorError> {
        let time_ms = event.wall_time_ms;
        match decode_response(Generation::Gen4, payload).map_err(protocol_error)? {
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
                "whoop4-identity",
                &format!("strap {device_name}, firmware {firmware:?}"),
            ),
            CommandResponse::Clock { unix } => self.diagnostic(
                event,
                DiagnosticLevel::Info,
                "whoop4-clock",
                &format!("strap RTC reads {unix}"),
            ),
            CommandResponse::DataRange { oldest, newest } => self.diagnostic(
                event,
                DiagnosticLevel::Info,
                "whoop4-data-range",
                &format!("banked history spans {oldest:?}..{newest:?}"),
            ),
            _ => Ok(empty()),
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
            Ok(samples) => bodies.push(ActionBody::EmitSamples {
                batch_id: BatchId(self.next_operation),
                samples,
            }),
            Err(error) => bodies.push(ActionBody::EmitDiagnostic {
                level: DiagnosticLevel::Warning,
                code: "whoop4-decode".to_owned(),
                message: format!("WHOOP 4.0 payload rejected: {error:?}"),
            }),
        }
        if bodies.is_empty() {
            Ok(empty())
        } else {
            self.actions(event, bodies)
        }
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
        build_command(Generation::Gen4, seq, opcode, body).map_err(protocol_error)
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
        if bytes.len() != SNAPSHOT_LEN || bytes.get(..3) != Some(b"W4\x01") {
            return Err(ConnectorError::InvalidWire(
                "WHOOP 4.0 state snapshot is malformed".to_owned(),
            ));
        }
        self.phase = phase(bytes[3])?;
        self.subscriptions = bytes[4];
        self.command_seq = bytes[5];
        let mut operation = [0u8; 8];
        operation.copy_from_slice(&bytes[6..14]);
        self.next_operation = u64::from_le_bytes(operation).max(1);
        self.history_retries = bytes[14];
        Ok(())
    }
}

fn empty() -> ActionBatch {
    ActionBatch {
        actions: Vec::new(),
    }
}

fn is_gen4(service_uuids: &[String], name: Option<&str>) -> bool {
    has_uuid(service_uuids, GEN4_SERVICE)
        && name.is_some_and(|value| value == "4.0" || value.starts_with("WHOOP 4.0"))
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
        _ => None,
    }
}

fn phase(value: u8) -> Result<Phase, ConnectorError> {
    match value {
        0 => Ok(Phase::Idle),
        1 => Ok(Phase::Scanning),
        2 => Ok(Phase::Connecting),
        3 => Ok(Phase::Discovering),
        4 => Ok(Phase::Subscribing),
        5 => Ok(Phase::Configuring),
        6 => Ok(Phase::Streaming),
        7 => Ok(Phase::Historical),
        8 => Ok(Phase::Suspended),
        9 => Ok(Phase::Disconnected),
        _ => Err(ConnectorError::InvalidWire(
            "WHOOP 4.0 state phase is unknown".to_owned(),
        )),
    }
}

fn protocol_error(error: whoop_protocol::ProtocolError) -> ConnectorError {
    ConnectorError::InvalidWire(format!("WHOOP protocol error: {error:?}"))
}

fn streams() -> Vec<String> {
    [
        "heart-rate",
        "pulse-interval",
        "gravity",
        "spo2-raw",
        "skin-temp-raw",
        "resp-raw",
        "battery-soc",
        "wrist-state",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

export_connector!(Whoop4Connector);

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
        TransportCapability::Discover,
        TransportCapability::Subscribe,
        TransportCapability::Read,
        TransportCapability::Write,
    ];
    Ok(Manifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        connector_id: ConnectorId::new(CONNECTOR_ID)?,
        version: "1.0.2".to_owned(),
        display_name: "WHOOP 4.0".to_owned(),
        description: "Local WHOOP 4.0 connector; deep protocol remains hardware-unverified"
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
            id: "whoop4".to_owned(),
            name_prefixes: vec!["WHOOP 4.0".to_owned(), "4.0".to_owned()],
            service_uuids: vec![GEN4_SERVICE.to_owned()],
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
            id: "whoop4-custom".to_owned(),
            uuid: GEN4_SERVICE.to_owned(),
            characteristics: vec![
                characteristic(
                    COMMAND_ID,
                    "61080002-8d6d-82b8-614a-1c8cb0f8dcc6",
                    vec![CharacteristicProperty::Write],
                    true,
                ),
                characteristic(
                    COMMAND_RESPONSE_ID,
                    "61080003-8d6d-82b8-614a-1c8cb0f8dcc6",
                    vec![CharacteristicProperty::Notify],
                    false,
                ),
                characteristic(
                    EVENTS_ID,
                    "61080004-8d6d-82b8-614a-1c8cb0f8dcc6",
                    vec![CharacteristicProperty::Notify],
                    false,
                ),
                characteristic(
                    DATA_ID,
                    "61080005-8d6d-82b8-614a-1c8cb0f8dcc6",
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
                service_uuids: vec![GEN4_SERVICE.to_owned(), "180d".to_owned()],
                manufacturer_ids: Vec::new(),
            },
        }],
    };
    let mut cases = vec![FixtureCase {
        name: "activate-gen4-scan".to_owned(),
        initial_state: Vec::new(),
        events: vec![event],
        expected: vec![expected],
        expected_state_hash: [
            0x23, 0xf4, 0xcf, 0x61, 0x99, 0x31, 0x3d, 0xaa, 0xf7, 0x4f, 0x5f, 0x95, 0x1f, 0x1f,
            0x4e, 0xca, 0x4a, 0x51, 0x27, 0x5c, 0x91, 0x82, 0x14, 0x40, 0xec, 0x8c, 0x6c, 0x4b,
            0x32, 0x53, 0x3e, 0xd9,
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
    let history_end = {
        let cursor = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut payload = vec![0x31, 9, 2];
        payload.extend_from_slice(&[0; 10]);
        payload.extend_from_slice(&cursor);
        fixture_gen4_frame(&payload)?
    };
    let record = fixture_hex(include_str!(
        "../../../crates/whoop-protocol/tests/fixtures/whoop_rs_gen4_v24.hex"
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
                        bytes: fixture_gen4_frame(&[0x24, 1, 34, 0])?,
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
                    bytes: corrupt_gen4_frame()?,
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
    let mut driver = TestDriver::new(Whoop4Connector::default());
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
    let expected_state_hash = driver.snapshot_hash()?;
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
    let v24 = fixture_hex(include_str!(
        "../../../crates/whoop-protocol/tests/fixtures/whoop_rs_gen4_v24.hex"
    ))?;
    let v25 = fixture_hex(include_str!(
        "../../../crates/whoop-protocol/tests/fixtures/whoop_rs_gen4_v25.hex"
    ))?;
    let time_v24 = 1_780_928_574_000;
    let v24_samples = vec![
        wire_sample("heart-rate", 109_000_000, time_v24, 0, "beats-per-minute"),
        wire_sample("pulse-interval", 555_000_000, time_v24, 0, "milliseconds"),
        wire_sample("pulse-interval", 564_000_000, time_v24, 1, "milliseconds"),
        wire_sample("gravity", -403_115, time_v24, 0, "milli-g"),
        wire_sample("gravity", 450_591, time_v24, 1, "milli-g"),
        wire_sample("gravity", 872_478, time_v24, 2, "milli-g"),
        wire_sample("spo2-raw", 592_000_000, time_v24, 0, "counts"),
        wire_sample("spo2-raw", 612_000_000, time_v24, 1, "counts"),
        wire_sample("skin-temp-raw", 861_000_000, time_v24, 0, "counts"),
        wire_sample("resp-raw", 3_073_000_000, time_v24, 0, "counts"),
    ];
    let mut fixtures = vec![record_fixture(
        "real-gen4-v24",
        v24.clone(),
        v24_samples.clone(),
    )?];
    let mut payload_v12 = decode_frame(Generation::Gen4, &v24).map_err(protocol_error)?;
    payload_v12[1] = 12;
    fixtures.push(record_fixture(
        "mapped-gen4-v12",
        fixture_gen4_frame(&payload_v12)?,
        v24_samples,
    )?);
    fixtures.push(record_fixture(
        "real-gen4-v25",
        v25,
        vec![
            wire_sample("gravity", 942_688, 1_781_202_813_000, 0, "milli-g"),
            wire_sample("gravity", 55_664, 1_781_202_813_000, 1, "milli-g"),
            wire_sample("gravity", 0, 1_781_202_813_000, 2, "milli-g"),
        ],
    )?);
    for version in [5, 7, 9] {
        let mut payload = vec![47, version, 0x80];
        let mut body = vec![0u8; 20];
        body[4..8].copy_from_slice(&1_780_000_000u32.to_le_bytes());
        body[14] = 63;
        body[15] = 2;
        body[16..18].copy_from_slice(&800u16.to_le_bytes());
        body[18..20].copy_from_slice(&810u16.to_le_bytes());
        payload.extend(body);
        fixtures.push(record_fixture(
            &format!("mapped-gen4-v{version}"),
            fixture_gen4_frame(&payload)?,
            vec![
                wire_sample(
                    "heart-rate",
                    63_000_000,
                    1_780_000_000_000,
                    0,
                    "beats-per-minute",
                ),
                wire_sample(
                    "pulse-interval",
                    800_000_000,
                    1_780_000_000_000,
                    0,
                    "milliseconds",
                ),
                wire_sample(
                    "pulse-interval",
                    810_000_000,
                    1_780_000_000_000,
                    1,
                    "milliseconds",
                ),
            ],
        )?);
    }
    Ok(fixtures)
}

fn record_fixture(
    name: &str,
    bytes: Vec<u8>,
    samples: Vec<WireSample>,
) -> Result<FixtureCase, ConnectorError> {
    notification_fixture(name, DATA_ID, bytes, samples)
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
                wire_sample("pulse-interval", 800_000_000, time, 0, "milliseconds"),
            ],
        )?,
        notification_fixture(
            "custom-realtime",
            DATA_ID,
            fixture_gen4_frame(&realtime)?,
            vec![
                wire_sample("heart-rate", 64_000_000, time, 0, "beats-per-minute"),
                wire_sample("pulse-interval", 800_000_000, time, 0, "milliseconds"),
                wire_sample("pulse-interval", 810_000_000, time, 1, "milliseconds"),
            ],
        )?,
        notification_fixture(
            "battery-event",
            EVENTS_ID,
            fixture_gen4_frame(&battery)?,
            vec![wire_sample("battery-soc", 81_200_000, time, 0, "percent")],
        )?,
        notification_fixture(
            "wrist-event",
            EVENTS_ID,
            fixture_gen4_frame(&wrist)?,
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
    let expected = ActionBatch {
        actions: vec![ConnectorAction {
            connector_id: ConnectorId::new(CONNECTOR_ID)?,
            session_id: SessionId(1),
            caused_by: EventSequence(1),
            cancellation_generation: CancellationGeneration(0),
            operation_id: OperationId(1),
            deadline_token: TimerToken(1),
            body: ActionBody::EmitSamples {
                batch_id: BatchId(1),
                samples: samples.clone(),
            },
        }],
    };
    Ok(FixtureCase {
        name: name.to_owned(),
        initial_state: streaming_fixture_state(),
        events: vec![event],
        expected: vec![expected],
        expected_state_hash: [
            0x9a, 0xb9, 0x6e, 0xd2, 0x9e, 0x93, 0x27, 0xd3, 0x1e, 0xf2, 0x9f, 0x60, 0xfb, 0x48,
            0xa3, 0x16, 0x5b, 0xf6, 0xce, 0x45, 0x44, 0x0a, 0x19, 0xfc, 0x63, 0xfb, 0xbc, 0x88,
            0xcf, 0x10, 0xa8, 0x86,
        ],
        max_fuel: 1_000_000,
        expected_samples: Some(samples),
        expected_diagnostics: None,
    })
}

fn streaming_fixture_state() -> Vec<u8> {
    vec![
        0x57,
        0x34,
        1,
        Phase::Streaming as u8,
        0x0f,
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

/// A complete frame whose payload CRC fails. A merely truncated notification is no longer
/// malformed: the deframer holds it until the rest of the frame arrives.
fn corrupt_gen4_frame() -> Result<Vec<u8>, ConnectorError> {
    let mut frame = fixture_gen4_frame(&[0x2f, 24, 0, 0])?;
    if let Some(last) = frame.last_mut() {
        *last ^= 0xff;
    }
    Ok(frame)
}

fn fixture_gen4_frame(payload: &[u8]) -> Result<Vec<u8>, ConnectorError> {
    let declared = u16::try_from(payload.len() + 4)
        .map_err(|_| ConnectorError::InvalidWire("fixture payload is too large".to_owned()))?;
    let length = declared.to_le_bytes();
    let mut frame = vec![0xaa, length[0], length[1], 0];
    frame[3] = whoop_protocol::crc8(&frame[1..3]);
    frame.extend_from_slice(payload);
    frame.extend_from_slice(&whoop_protocol::crc32(payload).to_le_bytes());
    Ok(frame)
}
