# Migration plan

The authoritative cross-repository work packets are in
[`maverick/docs/plans/active/wasm-connectors.md`](https://github.com/sennnen/maverick/blob/main/docs/plans/active/wasm-connectors.md).
This file records connector-repository ownership and deletion conditions so work here never drifts
back to data-only folders or compiled app extensions.

## Current audit

WC-P12 removed the two legacy JSON folders and their structural parser. `tools/validate.py` now
checks only the public-SDK workspace, packaged metadata, deterministic Wasm outputs, and signed
artifact fixture reports. Device implementation lives in `connectors/whoop4` and
`connectors/whoop5`; Maverick installs their `.mavconn` bytes without a compiled registration.

Reference comparison at `tanarchytan/whoop-rs@375af9c` shows current manifests/compiled codec are
missing or stale around advertisement/identity reads, proven bond order, service discovery,
confirmed hello, response collection, full history driver, 8-second inactivity abort, ACK-after-
persist sequencing, reconnect policy, raw/deep flows, and dangerous-command gates. Gen5 manifest has
ten enable flags while reference code has sixteen. `force_trim` is listed in legacy manifests but is
forbidden by the reference safety model. Each difference requires a fixture or hardware
adjudication.

## Repository packet ownership

- **WC-P3 (complete):** SDK-consumer workspace, deterministic pack/inspect/validate/test tools,
  frozen schema registry, and a device-neutral native/Wasm template.
- **WC-P8 (complete):** port/adjudicate pure WHOOP reference logic and provenance fixtures; exclude
  desktop, async transport, persistence, FFI, and analytics.
- **WC-P9 (complete):** standalone WHOOP 4.0 SDK project, externally signed test artifact, expanded
  by WC-P11 to fourteen parity-profiled fixtures, and install proof through the public runtime.
- **WC-P10 (complete):** standalone paired WHOOP 5.0/MG SDK project, externally signed test
  artifact, expanded by WC-P11 to twelve parity-profiled fixtures, and explicit deep-stream
  uncertainty.
- **WC-P11 (complete):** frozen artifact/input/action/sample/state hashes, native/Wasm fixture
  equality, history/restart/malformed traces, and mobile fuel/memory profiles for both artifacts.
- **WC-P12 (complete):** switched replay and app core to signed artifacts; deleted the legacy JSON
  folders/parser and every compiled registration instruction. The validator remains for the SDK
  workspace and packaged artifact gates only.
- **WC-P15:** add publishing and signed registry index flow without networking in core or secrets in
  this repository.
- **WC-P16:** full dead-code/dependency/doc/fixture audit and focused bug fixes.

## Cleanup searches

Each migration packet searches this repository for top-level legacy `manifest.json`, compiled
registration terms,
`mav-connector-whoop`, Maverick path dependencies, `btleplug`, `tokio`, private-key extensions,
release JKS references, obsolete validator commands, temporary parity markers, and duplicate native/
Wasm paths. Matches must be removed or classified in the packet decision log.

## Rollback

Before WC-P12, packaged artifacts are additive test outputs and the old runtime remains active.
WC-P12 is one switch/deletion unit and can revert to the prior binary/manifest path if final local
validation fails. Connector install DB migrations remain forward-readable. After WC-P12, rollback is
artifact/version rollback through the public installer, not resurrection of compiled connector code.
