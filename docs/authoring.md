# Authoring a connector

Status: WC-P3 SDK, template, and structural tools implemented. Wasm execution joins `mavconn-test`
in WC-P4; device migrations and release publishing remain later packets.

## Workflow

1. Create a standalone Rust project against released `mav-connector-sdk`; never use a path into
   Maverick internals.
2. Declare stable connector/publisher ids, device advertisement rules, services/characteristics,
   capabilities, ABI/core ranges, state schema, and a host-defined resource profile.
3. Implement a deterministic state machine: receive normalized events, update private state, return
   bounded declarative actions. Do not call a radio, OS API, filesystem, network, clock, random
   source, thread, or process.
4. Write native unit tests and scripted protocol-state tests before code. Each asserts exact actions,
   samples, diagnostics, state hash, or typed failure.
5. Add bounded golden fixtures with provenance and confidence. Malformed input, disconnect,
   cancellation, timeout, restart, state corruption, and resource limits are required.
6. Compile `wasm32-unknown-unknown`, run native SDK tests, and run `mavconn-test`; full native/Wasm
   execution parity becomes blocking when WC-P4 lands the bounded interpreter.
7. Package deterministic metadata and fixtures with `mavconn-pack`; sign through an external
   publisher signer; inspect and validate the resulting one-file artifact.
8. Test installation, update, downgrade refusal, rollback, revocation, and uninstall without either
   mobile frontend.
9. Publish the exact digest-addressed bytes directly or through a signed registry entry. Publishing
   never changes Maverick.

Before the SDK release is available from its registry, maintainers can validate the exact package
shape without committing a path dependency:

```text
python3 tools/validate.py \
  --sdk-path ../maverick/core/crates/mav-connector-sdk \
  --tool-dir ../maverick/core/target/debug
```

The deep gate runs format, Clippy, native tests, two identical release Wasm builds, canonical
metadata generation, and two identical unsigned package builds. Production finalization is a second
`mavconn-pack` phase that receives only external signature and public-key bytes.

## ABI boundary

V1 exports allocation/deallocation, version, initialization, event handling, and state snapshot
functions. Messages are deterministic CBOR. SDK macros own pointer/length glue; connector authors do
not hand-roll it. V1 imports nothing from the host.

Events cover lifecycle, advertisements, service discovery, pairing, subscriptions, reads/writes,
notifications, timers, disconnect/cancellation, state commit, and sample commit. Actions cover only
declared scan/connect/pair/discover/subscribe/read/write/disconnect operations, opaque timers,
connector-scoped state, normalized samples, diagnostics, and completion.

Core executes ordered actions. Emit/persist samples before returning a device acknowledgement. A
connector may implement a protocol retry; core enforces global resource, deadline, cancellation,
manifest capability, and lifecycle rules.

## What belongs in connector code

- advertisement interpretation and identity rules;
- device UUIDs, packet/framing formats, commands and responses;
- device-specific handshake/authentication/acknowledgement state;
- generation and firmware branches supported by evidence;
- history transfer and protocol-specific retry semantics;
- connector-local learned protocol state;
- normalization to SDK sample types and safe diagnostics.

Analytics, user health judgments, core storage queries, UI, native BLE objects, acquisition source
fetching, publisher trust decisions, and resource-policy decisions do not belong here.

## Evidence and tests

Every protocol fact carries source and confidence. A fact from private reference source may guide
code/tests but must not be copied wholesale into public docs. Hardware-verified evidence outranks
older inference only when capture provenance is traceable. Conflicts stay explicit until adjudicated.

Packaged self-tests include expected ordered events/actions, normalized samples, final state hash,
and maximum fuel. Installation tests run in a fresh namespace. Tests must cover invalid lengths,
sentinels, unknown versions/events, CRC failures, partial frames, duplicate/reordered events,
timeouts, cancellation, reconnect, and corrupted state.

## Signing and publishing

Use a dedicated Ed25519 publisher identity. Never reuse Android JKS, Apple distribution identity,
registry root, or another publisher key. Keep private material outside git and tool output. Rotation
uses old-key cross-signature or registry-root authorization; revocation records are signed and
versioned.

One `.mavconn` is valid on both platforms. Platform trust differs: iOS may permit only reviewed
official publishers while Android may allow explicit third-party trust. Do not fork artifacts or ABI
to accommodate policy.
