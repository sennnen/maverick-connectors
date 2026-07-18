# Migration plan

The authoritative cross-repository work packets are in
[`maverick/docs/plans/active/wasm-connectors.md`](https://github.com/sennnen/maverick/blob/main/docs/plans/active/wasm-connectors.md).
This file records connector-repository ownership and deletion conditions so work here never drifts
back to data-only folders or compiled app extensions.

## Current audit

At `dfb351d`, this repository has two JSON manifests and a Python structural validator. Both
manifests name compiled codec id `whoop`; they cannot install functional logic independently. README
and authoring docs instructed developers to add a crate under Maverick and register it in both edge
crates. That is the bundled architecture ADR-017 replaces.

Reference comparison at `tanarchytan/whoop-rs@375af9c` shows current manifests/compiled codec are
missing or stale around advertisement/identity reads, proven bond order, service discovery,
confirmed hello, response collection, full history driver, 8-second inactivity abort, ACK-after-
persist sequencing, reconnect policy, raw/deep flows, and dangerous-command gates. Gen5 manifest has
ten enable flags while reference code has sixteen. `force_trim` is listed in legacy manifests but is
forbidden by the reference safety model. Each difference requires a fixture or hardware
adjudication.

## Repository packet ownership

- **WC-P3:** create SDK-consumer workspace, deterministic pack/inspect/validate/test tools, and a
  device-neutral template. Legacy validator remains temporary.
- **WC-P8:** port/adjudicate pure WHOOP reference logic and provenance fixtures; exclude desktop,
  async transport, persistence, FFI, and analytics.
- **WC-P9:** create standalone WHOOP 4.0 SDK project and signed test artifact.
- **WC-P10:** create standalone WHOOP 5.0/MG SDK project and signed test artifact.
- **WC-P11:** provide artifact hashes, state traces, native/Wasm parity fixtures, and performance
  inputs to cross-platform proof.
- **WC-P12:** after proof and runtime switch, delete `whoop4/manifest.json`,
  `whoop5/manifest.json`, `tools/validate.py`, old folder-import docs/config, and every compiled-codec
  instruction. Useful facts/tests must already exist in packaged projects.
- **WC-P15:** add publishing and signed registry index flow without networking in core or secrets in
  this repository.
- **WC-P16:** full dead-code/dependency/doc/fixture audit and focused bug fixes.

## Cleanup searches

Each migration packet searches this repository for `manifest.json`, `codec`, `register_codec`,
`mav-connector-whoop`, Maverick path dependencies, `btleplug`, `tokio`, private-key extensions,
release JKS references, obsolete validator commands, temporary parity markers, and duplicate native/
Wasm paths. Matches must be removed or classified in the packet decision log.

## Rollback

Before WC-P12, packaged artifacts are additive test outputs and the old runtime remains active.
WC-P12 is one switch/deletion unit and can revert to the prior binary/manifest path if final local
validation fails. Connector install DB migrations remain forward-readable. After WC-P12, rollback is
artifact/version rollback through the public installer, not resurrection of compiled connector code.
