pub mod decode;

use decode::{decode_payload, decode_standard_heart_rate};
use mav_connector_sdk::abi::*;
use mav_connector_sdk::{
    artifact_metadata, export_connector, ActionBuilder, Connector, ConnectorError, TestDriver,
};
use sha2::{Digest, Sha256};
use whoop_protocol::{
    build_command, decode_control, decode_frame, get_data_range, history_ack, request_history,
    Control, ControlResult, Generation,
};

pub const CONNECTOR_ID: &str = "dev.maverick.whoop5";
pub const GEN5_SERVICE: &str = "fd4b0001-cce1-4033-93ce-002d5875f58a";
const COMMAND_ID: &str = "command";
const STANDARD_HR_ID: &str = "standard-heart-rate";
const COMMAND_RESPONSE_ID: &str = "command-response";
const EVENTS_ID: &str = "events";
const DATA_ID: &str = "data";
const DATA_SECONDARY_ID: &str = "data-secondary";
const ALL_SUBSCRIPTIONS: u8 = 0x1f;
const IDLE_TIMER: TimerToken = TimerToken(200);
const RESPONSE_TIMER: TimerToken = TimerToken(201);
const SNAPSHOT_LEN: usize = 17;
const FEATURE_FLAGS: [&str; 10] = [
    "enable_r22_packets",
    "enable_r22_v2_packets",
    "enable_r22_v3_packets",
    "enable_r22_v5_packets",
    "enable_r22_v6_packets",
    "enable_r22_v8_packets",
    "make_hrfm_visible",
    "hr_ch_switching",
    "enable_passive_strap_fit_gen5",
    "enable_sig11_during_sleep",
];

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
                        DATA_SECONDARY_ID,
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
                self.phase = Phase::Historical;
                self.history_retries = 0;
                let seq = self.take_command_seq();
                let command = get_data_range(Generation::Gen5, seq).map_err(protocol_error)?;
                self.write_with_timeout(&event, command)
            }
            EventBody::TimerFired { token } if *token == RESPONSE_TIMER => {
                if self.phase != Phase::Historical || self.history_retries >= 1 {
                    self.phase = Phase::Streaming;
                    return self.diagnostic(
                        &event,
                        DiagnosticLevel::Warning,
                        "whoop5-history-timeout",
                        "historical response timed out",
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
                let command = self.command(117, &[])?;
                self.write(event, command)
            }
            1 => {
                self.config_step = 2;
                let command = self.command(118, &[])?;
                self.write(event, command)
            }
            2..=11 => {
                let index = usize::from(self.config_step - 2);
                let body = feature_flag_body(FEATURE_FLAGS[index])?;
                self.config_step += 1;
                let command = self.command(120, &body)?;
                self.write(event, command)
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
                            delay_ms: 60_000,
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
            return self.emit_or_diagnose(event, decode_standard_heart_rate(bytes, wall));
        }
        let payload = match decode_frame(Generation::Gen5, bytes) {
            Ok(payload) => payload,
            Err(error) => {
                return self.diagnostic(
                    event,
                    DiagnosticLevel::Warning,
                    "whoop5-frame",
                    &format!("malformed WHOOP 5.0/MG frame: {error:?}"),
                )
            }
        };
        match decode_control(&payload).map_err(protocol_error)? {
            Some(control) => self.control(event, control),
            None => self.emit_or_diagnose(event, decode_payload(&payload)),
        }
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
                Ok(empty())
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
                    vec![ActionBody::SetTimer {
                        token: IDLE_TIMER,
                        delay_ms: 60_000,
                    }],
                )
            }
            Control::Response { .. } | Control::MetadataUnknown { .. } => Ok(empty()),
        }
    }

    fn emit_or_diagnose(
        &mut self,
        event: &ConnectorEvent,
        decoded: Result<Vec<WireSample>, decode::DecodeError>,
    ) -> Result<ActionBatch, ConnectorError> {
        match decoded {
            Ok(samples) if samples.is_empty() => Ok(empty()),
            Ok(samples) => {
                let bodies = samples
                    .chunks(MAX_SAMPLES_PER_ACTION)
                    .enumerate()
                    .map(|(index, chunk)| ActionBody::EmitSamples {
                        batch_id: BatchId(self.next_operation.saturating_add(index as u64)),
                        samples: chunk.to_vec(),
                    })
                    .collect();
                self.actions(event, bodies)
            }
            Err(error) => self.diagnostic(
                event,
                DiagnosticLevel::Warning,
                "whoop5-decode",
                &format!("WHOOP 5.0/MG payload rejected: {error:?}"),
            ),
        }
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
        DATA_SECONDARY_ID => Some(16),
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

fn feature_flag_body(name: &str) -> Result<[u8; 40], ConnectorError> {
    if name.len() > 32 || !name.is_ascii() {
        return Err(ConnectorError::InvalidWire(
            "WHOOP feature flag name exceeds its field".to_owned(),
        ));
    }
    let mut body = [0u8; 40];
    body[..name.len()].copy_from_slice(name.as_bytes());
    body[32] = 1;
    Ok(body)
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
        version: "1.0.0".to_owned(),
        display_name: "WHOOP 5.0 / MG".to_owned(),
        description: "Local WHOOP 5.0/MG connector; deep availability remains unverified"
            .to_owned(),
        publisher_key_id: "maverick-whoop-test".to_owned(),
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
                    DATA_SECONDARY_ID,
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
        sdk_version: "0.1.0".to_owned(),
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
    Ok(vec![
        native_parity_fixture(
            "history-cursor-retry",
            streaming_fixture_state(),
            vec![
                fixture_event(1, EventBody::TimerFired { token: IDLE_TIMER })?,
                fixture_event(
                    2,
                    EventBody::Notification {
                        characteristic_id: COMMAND_RESPONSE_ID.to_owned(),
                        bytes: fixture_gen5_frame(&[0x24, 1, 34, 1])?,
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
                    bytes: vec![0xaa, 0x01],
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
        wire_sample("skin-temp", 3_057_000_000, time_v18, 0, "degrees-celsius"),
        wire_sample("step-count", 50_000_000, time_v18, 0, "count"),
        wire_sample("activity-class", 0, time_v18, 0, "code"),
        wire_sample("sleep-state-raw", 0, time_v18, 0, "code"),
        wire_sample("signal-quality", 255_000_000, time_v18, 0, "percent"),
    ];
    let ppg_values = [
        292, 306, 463, 553, 9, -1550, -1952, -1503, -1082, -791, -343, -346, -352, -313, -162,
        -133, 100, 102, 252, 344, 327, 460, 291, -902,
    ];
    let v26_samples = ppg_values
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
            DATA_SECONDARY_ID,
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
