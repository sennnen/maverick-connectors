use mav_connector_sdk::abi::*;
use mav_connector_sdk::{artifact_metadata, export_connector, Connector, ConnectorError};

#[derive(Default)]
struct TemplateConnector;

impl Connector for TemplateConnector {
    fn handle(&mut self, _event: ConnectorEvent) -> Result<ActionBatch, ConnectorError> {
        Ok(ActionBatch {
            actions: Vec::new(),
        })
    }

    fn snapshot(&self) -> Result<Vec<u8>, ConnectorError> {
        Ok(Vec::new())
    }
}

export_connector!(TemplateConnector);

artifact_metadata! {
    pub fn metadata() {
        manifest: Manifest {
            schema: MANIFEST_SCHEMA.to_owned(),
            connector_id: ConnectorId::new("org.example.template")?,
            version: "1.0.0".to_owned(),
            display_name: "Template".to_owned(),
            description: "Device-neutral connector template".to_owned(),
            publisher_key_id: "template-test-key".to_owned(),
            abi: AbiRange { major: 1, min_minor: 0, max_minor: 0 },
            core: CoreRange { min_version: "0.1.0".to_owned(), max_version: None },
            state_schema: 1,
            artifact_limits_profile: LimitsProfileId::new("mobile-v1")?,
            device_families: vec![DeviceFamily {
                id: "template".to_owned(),
                name_prefixes: vec!["Template".to_owned()],
                service_uuids: vec!["180d".to_owned()],
                manufacturer_id: None,
                manufacturer_mask: Vec::new(),
                manufacturer_value: Vec::new(),
            }],
            services: vec![ServiceDecl {
                id: "service".to_owned(),
                uuid: "180d".to_owned(),
                characteristics: vec![CharacteristicDecl {
                    id: "data".to_owned(),
                    uuid: "2a37".to_owned(),
                    properties: vec![CharacteristicProperty::Notify],
                    sensitive: false,
                    confirmed_write_required: false,
                }],
            }],
            capabilities: vec![CapabilityDecl {
                stream: "heart-rate".to_owned(),
                transport: vec![TransportCapability::Subscribe],
            }],
            permissions: vec![Permission::Ble],
            entrypoints: Entrypoints::default(),
            fixture_set_hash: [0; 32],
            update: UpdatePolicy {
                channel: "stable".to_owned(),
                downgrade: DowngradePolicy::Reject,
            },
        },
        abi: AbiDescriptor {
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
            ].map(str::to_owned).to_vec(),
            required_imports: Vec::new(),
            wasm_features: vec![
                WasmFeature::MutableGlobals,
                WasmFeature::SignExtension,
                WasmFeature::BulkMemory,
            ],
            sdk_version: "0.1.0".to_owned(),
        },
        fixtures: FixtureSet {
            schema: FIXTURES_SCHEMA.to_owned(),
            cases: vec![FixtureCase {
                name: "activate".to_owned(),
                initial_state: Vec::new(),
                events: vec![ConnectorEvent {
                    connector_id: ConnectorId::new("org.example.template")?,
                    session_id: SessionId(1),
                    sequence: EventSequence(1),
                    cancellation_generation: CancellationGeneration(0),
                    wall_time_ms: None,
                    body: EventBody::Activate,
                }],
                expected: vec![ActionBatch { actions: Vec::new() }],
                expected_state_hash: [
                    0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14,
                    0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
                    0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c,
                    0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
                ],
                max_fuel: 1_000_000,
                expected_samples: None,
                expected_diagnostics: None,
            }],
        },
    }
}
