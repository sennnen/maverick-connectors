# WHOOP 4.0 connector

Standalone public-SDK connector for WHOOP 4.0. The state machine scans only the gen4 identity,
uses the unbonded standard heart-rate path, subscribes to the admitted custom channels, writes the
captured gen4 hello opcode, and runs non-destructive historical offload. Opcode 25 (`FORCE_TRIM`)
cannot be built by the shared protocol library and is absent from this connector.

Native tests pin advertisement/model rejection, subscription and hello ordering, standard and
custom realtime data, events, every admitted v5/v7/v9/v12/v24/v25 record route, bounded history
retry, exact cursor acknowledgement, disconnect, and state restore. Fourteen embedded artifact tests
replay activation, standard/custom realtime, battery/wrist events, and every admitted historical
record version under Wasm, plus history cursor retry, state restart, and malformed input. The
generated `parity-v1.json` freezes canonical input/action/sample/state hashes and per-call
fuel/linear-memory profiles. The v24/v25 bytes are
real `[WRS]` captures; v5/v7/v9 remain `[PROV]`, and v12 shares the real v24 layout. The 4.0 custom
handshake and absolute temperature scale still await this project's own hardware.

`package-test.json` contains only a public test key and a detached signature made by a temporary
external signer. No private key is stored. Deep validation rebuilds identical Wasm, reconstructs
the signed `.mavconn`, verifies its digest, runs every fixture, and reproduces the parity report.
