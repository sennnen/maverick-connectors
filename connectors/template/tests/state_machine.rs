#![allow(clippy::expect_used, clippy::panic)]

use mav_connector_sdk::abi::*;
use mav_connector_sdk::{Connector, ConnectorError, TestDriver};

#[derive(Default)]
struct EmptyConnector;

impl Connector for EmptyConnector {
    fn handle(&mut self, _event: ConnectorEvent) -> Result<ActionBatch, ConnectorError> {
        Ok(ActionBatch {
            actions: Vec::new(),
        })
    }

    fn snapshot(&self) -> Result<Vec<u8>, ConnectorError> {
        Ok(Vec::new())
    }
}

#[test]
fn template_asserts_exact_native_actions_and_state() {
    let event = ConnectorEvent {
        connector_id: ConnectorId::new("org.example.template").expect("connector id"),
        session_id: SessionId(1),
        sequence: EventSequence(1),
        cancellation_generation: CancellationGeneration(0),
        wall_time_ms: None,
        body: EventBody::Activate,
    };
    let mut driver = TestDriver::new(EmptyConnector);
    assert_eq!(
        driver.drive(event),
        Ok(ActionBatch {
            actions: Vec::new()
        })
    );
    assert_eq!(driver.snapshot(), Ok(Vec::new()));
}
