//! Generic HR Monitor: any device that speaks the Bluetooth SIG Heart Rate Service.
//!
//! Chest straps, arm bands, most gym equipment and a lot of watches all publish 0x180D, so this
//! one connector covers hardware nobody has to write a driver for. It is also the only source of
//! genuine heart-rate variability available today: a chest strap times its intervals from the R
//! wave through electrodes, and the strap says so itself in the Body Sensor Location
//! characteristic. That byte is what decides which interval stream the beats are published on —
//! `rr-interval` for an electrode, `pulse-interval` for anything optical — so the distinction is
//! read off the device rather than assumed.
//!
//! The radio is left alone as much as possible: everything arrives by notification, nothing polls,
//! and the two static facts (sensor site, battery) are read once on connect with battery kept
//! current by notification where the device supports it.

use ble_sig::{HeartRateMeasurement, SensorSite};
use mav_connector_sdk::abi::*;
use mav_connector_sdk::{
    artifact_metadata, export_connector, ActionBuilder, Connector, ConnectorError,
};

pub const CONNECTOR_ID: &str = "dev.maverick.generic-hr";
pub const CONNECTOR_VERSION: &str = "1.0.0";

const MEASUREMENT_ID: &str = "heart-rate-measurement";
const SITE_ID: &str = "body-sensor-location";
const BATTERY_ID: &str = "battery-level";

/// How long a link may go without a notification before the connector gives up and lets the host
/// reconnect. Chest straps notify about once a second, so a minute of silence is a dead link
/// rather than a quiet one — and waiting is cheaper for both batteries than probing.
const SILENCE_TIMEOUT_MS: u64 = 60_000;
const SILENCE_TIMER: TimerToken = TimerToken(1);

const SNAPSHOT_TAG: &[u8; 2] = b"GH";
const SNAPSHOT_LEN: usize = 12;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
enum Phase {
    #[default]
    Idle = 0,
    Scanning = 1,
    Connecting = 2,
    Discovering = 3,
    Subscribing = 4,
    Streaming = 5,
    Disconnected = 6,
}

#[derive(Debug)]
pub struct GenericHeartRateConnector {
    phase: Phase,
    /// Operation ids are one-based: the host rejects a zero id, so counting from zero would make
    /// the connector's very first action invalid.
    next_operation: u64,
    /// Where the device says its sensor sits, once it has been read. Until then beats are
    /// published as optical, which is the claim that cannot be wrong in the direction that matters.
    site: Option<SensorSite>,
}

impl Default for GenericHeartRateConnector {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            next_operation: 1,
            site: None,
        }
    }
}

impl Connector for GenericHeartRateConnector {
    fn handle(&mut self, event: ConnectorEvent) -> Result<ActionBatch, ConnectorError> {
        match &event.body {
            EventBody::Activate | EventBody::Resume => {
                self.phase = Phase::Scanning;
                self.actions(
                    &event,
                    vec![ActionBody::StartScan {
                        service_uuids: vec![ble_sig::HEART_RATE_SERVICE.to_owned()],
                        manufacturer_ids: Vec::new(),
                    }],
                )
            }
            EventBody::Advertisement {
                address,
                service_uuids,
                ..
            } if self.phase == Phase::Scanning && advertises_heart_rate(service_uuids) => {
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
            EventBody::Connected { .. } => {
                self.phase = Phase::Discovering;
                self.actions(&event, vec![ActionBody::DiscoverServices])
            }
            EventBody::ServicesDiscovered { service_uuids }
                if advertises_heart_rate(service_uuids) =>
            {
                self.phase = Phase::Subscribing;
                let mut plan = vec![
                    ActionBody::Subscribe {
                        characteristic_id: MEASUREMENT_ID.to_owned(),
                    },
                    ActionBody::Read {
                        characteristic_id: SITE_ID.to_owned(),
                    },
                ];
                if has_uuid(service_uuids, ble_sig::BATTERY_SERVICE) {
                    plan.push(ActionBody::Read {
                        characteristic_id: BATTERY_ID.to_owned(),
                    });
                    plan.push(ActionBody::Subscribe {
                        characteristic_id: BATTERY_ID.to_owned(),
                    });
                }
                self.actions(&event, plan)
            }
            EventBody::ServicesDiscovered { .. } => self.diagnostic(
                &event,
                DiagnosticLevel::Error,
                "generic-hr-services",
                "the device did not offer the Heart Rate Service after connecting",
            ),
            EventBody::Subscribed { characteristic_id } if characteristic_id == MEASUREMENT_ID => {
                self.phase = Phase::Streaming;
                self.actions(
                    &event,
                    vec![
                        ActionBody::DeclareCapabilities {
                            streams: self.streams(),
                        },
                        ActionBody::SetTimer {
                            token: SILENCE_TIMER,
                            delay_ms: SILENCE_TIMEOUT_MS,
                        },
                    ],
                )
            }
            EventBody::ReadResult {
                characteristic_id,
                bytes,
                ..
            } => self.read_result(&event, characteristic_id, bytes),
            EventBody::Notification {
                characteristic_id,
                bytes,
            } => self.notification(&event, characteristic_id, bytes),
            EventBody::TimerFired { token } if *token == SILENCE_TIMER => {
                self.phase = Phase::Disconnected;
                self.actions(
                    &event,
                    vec![
                        ActionBody::EmitDiagnostic {
                            level: DiagnosticLevel::Warning,
                            code: "generic-hr-silent".to_owned(),
                            message: "no measurement for a minute; dropping the link".to_owned(),
                        },
                        ActionBody::Disconnect,
                    ],
                )
            }
            EventBody::Disconnected { .. } => {
                self.phase = Phase::Disconnected;
                Ok(empty())
            }
            EventBody::Deactivate | EventBody::Suspend | EventBody::Cancel { .. } => {
                self.phase = Phase::Disconnected;
                self.actions(
                    &event,
                    vec![
                        ActionBody::CancelTimer {
                            token: SILENCE_TIMER,
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
        bytes.extend_from_slice(SNAPSHOT_TAG);
        bytes.push(1);
        bytes.push(self.phase as u8);
        bytes.extend_from_slice(&self.next_operation.to_le_bytes());
        Ok(bytes)
    }
}

impl GenericHeartRateConnector {
    /// Which interval stream this device's beats belong on. Optical until the sensor says
    /// otherwise: publishing an optical pulse as heart-rate variability is the one error here that
    /// would put a wrong claim in front of the wearer.
    fn interval_stream(&self) -> &'static str {
        match self.site {
            Some(site) if site.is_electrical() => "rr-interval",
            _ => "pulse-interval",
        }
    }

    fn streams(&self) -> Vec<String> {
        [
            "heart-rate",
            self.interval_stream(),
            "skin-contact",
            "battery-soc",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    fn read_result(
        &mut self,
        event: &ConnectorEvent,
        characteristic_id: &str,
        bytes: &[u8],
    ) -> Result<ActionBatch, ConnectorError> {
        match characteristic_id {
            SITE_ID => {
                let code = bytes.first().copied().unwrap_or_default();
                self.site = SensorSite::from_code(code);
                let claim = match self.site {
                    Some(site) if site.is_electrical() => {
                        "electrode-timed beats: variability is reported as HRV"
                    }
                    Some(_) => "optically timed beats: variability is reported as PRV",
                    None => "unrecognised sensor location; assuming optical timing",
                };
                self.diagnostic(
                    event,
                    DiagnosticLevel::Info,
                    "generic-hr-sensor-site",
                    &format!("{:?} — {claim}", self.site),
                )
            }
            BATTERY_ID => self.battery(event, bytes),
            _ => Ok(empty()),
        }
    }

    fn notification(
        &mut self,
        event: &ConnectorEvent,
        characteristic_id: &str,
        bytes: &[u8],
    ) -> Result<ActionBatch, ConnectorError> {
        match characteristic_id {
            BATTERY_ID => self.battery(event, bytes),
            MEASUREMENT_ID => {
                let arrived_ms = event.wall_time_ms.ok_or_else(|| {
                    ConnectorError::InvalidWire(
                        "a heart-rate measurement arrived with no host time".to_owned(),
                    )
                })?;
                let measurement = ble_sig::decode_heart_rate(bytes).map_err(|error| {
                    ConnectorError::InvalidWire(format!(
                        "heart-rate measurement did not decode: {error:?}"
                    ))
                })?;
                let samples = self.samples(&measurement, arrived_ms);
                let mut plan = vec![ActionBody::SetTimer {
                    token: SILENCE_TIMER,
                    delay_ms: SILENCE_TIMEOUT_MS,
                }];
                if !samples.is_empty() {
                    plan.push(ActionBody::EmitSamples {
                        batch_id: BatchId(self.next_operation),
                        samples,
                    });
                }
                self.actions(event, plan)
            }
            _ => Ok(empty()),
        }
    }

    fn samples(&self, measurement: &HeartRateMeasurement, arrived_ms: i64) -> Vec<WireSample> {
        let mut samples = Vec::with_capacity(measurement.intervals_ms.len() + 2);
        if measurement.beats_per_minute > 0 {
            samples.push(sample(
                "heart-rate",
                i64::from(measurement.beats_per_minute),
                arrived_ms,
                0,
                "beats-per-minute",
            ));
        }
        if let Some(contact) = measurement.skin_contact {
            samples.push(sample(
                "skin-contact",
                i64::from(contact),
                arrived_ms,
                0,
                "boolean",
            ));
        }
        let stream = self.interval_stream();
        for (sequence, (at_ms, interval_ms)) in measurement
            .timed_intervals(arrived_ms)
            .into_iter()
            .enumerate()
        {
            samples.push(sample(
                stream,
                i64::from(interval_ms),
                at_ms,
                sequence as u32,
                "milliseconds",
            ));
        }
        samples
    }

    fn battery(
        &mut self,
        event: &ConnectorEvent,
        bytes: &[u8],
    ) -> Result<ActionBatch, ConnectorError> {
        let (Ok(percent), Some(at_ms)) = (ble_sig::decode_battery_level(bytes), event.wall_time_ms)
        else {
            return Ok(empty());
        };
        self.actions(
            event,
            vec![ActionBody::EmitSamples {
                batch_id: BatchId(self.next_operation),
                samples: vec![sample(
                    "battery-soc",
                    i64::from(percent),
                    at_ms,
                    0,
                    "percent",
                )],
            }],
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
}

fn sample(stream: &str, value: i64, device_time_ms: i64, sequence: u32, unit: &str) -> WireSample {
    WireSample {
        stream: stream.to_owned(),
        value_microunits: value.saturating_mul(1_000_000),
        device_time_ms: Some(device_time_ms),
        sequence,
        unit: unit.to_owned(),
    }
}

fn advertises_heart_rate(service_uuids: &[String]) -> bool {
    has_uuid(service_uuids, ble_sig::HEART_RATE_SERVICE)
}

fn has_uuid(service_uuids: &[String], wanted: &str) -> bool {
    service_uuids
        .iter()
        .any(|uuid| uuid.eq_ignore_ascii_case(wanted) || uuid.to_ascii_lowercase().contains(wanted))
}

fn empty() -> ActionBatch {
    ActionBatch {
        actions: Vec::new(),
    }
}

export_connector!(GenericHeartRateConnector);

artifact_metadata! {
    pub fn metadata() {
        manifest: manifest()?,
        abi: abi_descriptor(),
        fixtures: fixture_set()?,
    }
}

fn manifest() -> Result<Manifest, ConnectorError> {
    Ok(Manifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        connector_id: ConnectorId::new(CONNECTOR_ID)?,
        version: CONNECTOR_VERSION.to_owned(),
        display_name: "Generic HR Monitor".to_owned(),
        description: "Any chest strap, arm band or watch that speaks the standard Bluetooth \
                      heart-rate profile. Chest straps also give beat-to-beat intervals timed \
                      from the heart's electrical signal."
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
            id: "bluetooth-heart-rate".to_owned(),
            name_prefixes: Vec::new(),
            service_uuids: vec![ble_sig::HEART_RATE_SERVICE.to_owned()],
            manufacturer_id: None,
            manufacturer_mask: Vec::new(),
            manufacturer_value: Vec::new(),
        }],
        services: vec![
            ServiceDecl {
                id: "heart-rate".to_owned(),
                uuid: ble_sig::HEART_RATE_SERVICE.to_owned(),
                characteristics: vec![
                    CharacteristicDecl {
                        id: MEASUREMENT_ID.to_owned(),
                        uuid: ble_sig::HEART_RATE_MEASUREMENT.to_owned(),
                        properties: vec![CharacteristicProperty::Notify],
                        sensitive: false,
                        confirmed_write_required: false,
                    },
                    CharacteristicDecl {
                        id: SITE_ID.to_owned(),
                        uuid: ble_sig::BODY_SENSOR_LOCATION.to_owned(),
                        properties: vec![CharacteristicProperty::Read],
                        sensitive: false,
                        confirmed_write_required: false,
                    },
                ],
            },
            ServiceDecl {
                id: "battery".to_owned(),
                uuid: ble_sig::BATTERY_SERVICE.to_owned(),
                characteristics: vec![CharacteristicDecl {
                    id: BATTERY_ID.to_owned(),
                    uuid: ble_sig::BATTERY_LEVEL.to_owned(),
                    properties: vec![CharacteristicProperty::Read, CharacteristicProperty::Notify],
                    sensitive: false,
                    confirmed_write_required: false,
                }],
            },
        ],
        // Every capability carries the whole transport set this connector uses, because the host
        // checks each action against what the manifest signed and a scan is not covered by a
        // declaration that only mentions subscribing.
        capabilities: [
            "heart-rate",
            "rr-interval",
            "pulse-interval",
            "skin-contact",
            "battery-soc",
        ]
        .into_iter()
        .map(|stream| CapabilityDecl {
            stream: stream.to_owned(),
            transport: vec![
                TransportCapability::Scan,
                TransportCapability::Connect,
                TransportCapability::Discover,
                TransportCapability::Subscribe,
                TransportCapability::Read,
            ],
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

/// Every fixture is generated by driving the connector itself, so the expected actions and the
/// state digest cannot drift from the code they describe — the case pins behaviour, not a guess
/// about behaviour.
fn fixture_set() -> Result<FixtureSet, ConnectorError> {
    let session = |code: u8| -> Result<Vec<ConnectorEvent>, ConnectorError> {
        Ok(vec![
            event(1, EventBody::Activate)?,
            event(
                2,
                EventBody::Advertisement {
                    address: "AA:BB:CC:DD:EE:FF".to_owned(),
                    rssi: -60,
                    service_uuids: vec![ble_sig::HEART_RATE_SERVICE.to_owned()],
                    manufacturer_data: Vec::new(),
                    name: Some("HRM-Pro".to_owned()),
                },
            )?,
            event(3, EventBody::Connected { mtu: 23 })?,
            event(
                4,
                EventBody::ServicesDiscovered {
                    service_uuids: vec![
                        ble_sig::HEART_RATE_SERVICE.to_owned(),
                        ble_sig::BATTERY_SERVICE.to_owned(),
                    ],
                },
            )?,
            event(
                5,
                EventBody::ReadResult {
                    operation_id: OperationId(4),
                    characteristic_id: SITE_ID.to_owned(),
                    bytes: vec![code],
                },
            )?,
            event(
                6,
                EventBody::Subscribed {
                    characteristic_id: MEASUREMENT_ID.to_owned(),
                },
            )?,
            event(
                7,
                EventBody::Notification {
                    characteristic_id: MEASUREMENT_ID.to_owned(),
                    // flags 0x16: contact supported and detected, intervals present.
                    bytes: vec![0x16, 62, 0x33, 0x03, 0x00, 0x04],
                },
            )?,
        ])
    };
    Ok(FixtureSet {
        schema: FIXTURES_SCHEMA.to_owned(),
        cases: vec![
            derived("chest-strap-reports-electrical-intervals", session(1)?)?,
            derived("wrist-sensor-reports-optical-intervals", session(2)?)?,
            derived(
                "battery-level-notification",
                vec![
                    event(1, EventBody::Activate)?,
                    event(
                        2,
                        EventBody::Notification {
                            characteristic_id: BATTERY_ID.to_owned(),
                            bytes: vec![81],
                        },
                    )?,
                ],
            )?,
        ],
    })
}

fn event(sequence: u64, body: EventBody) -> Result<ConnectorEvent, ConnectorError> {
    Ok(ConnectorEvent {
        connector_id: ConnectorId::new(CONNECTOR_ID)?,
        session_id: SessionId(1),
        sequence: EventSequence(sequence),
        cancellation_generation: CancellationGeneration(0),
        wall_time_ms: Some(1_780_000_000_000),
        body,
    })
}

fn derived(name: &str, events: Vec<ConnectorEvent>) -> Result<FixtureCase, ConnectorError> {
    let mut driver = mav_connector_sdk::TestDriver::new(GenericHeartRateConnector::default());
    let mut expected = Vec::with_capacity(events.len());
    for event in &events {
        expected.push(driver.drive(event.clone())?);
    }
    Ok(FixtureCase {
        name: name.to_owned(),
        initial_state: Vec::new(),
        events,
        expected,
        expected_state_hash: driver.snapshot_hash()?,
        max_fuel: 1_000_000,
        expected_samples: None,
        expected_diagnostics: None,
    })
}

#[cfg(test)]
mod tests;
