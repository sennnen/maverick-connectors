# Connector template

This standalone device-neutral project consumes only released `mav-connector-sdk`. Implement the
event/action state machine in `src/lib.rs`; define bounded metadata and embedded fixture cases with
`artifact_metadata!`.

`tools/validate.py --sdk-path PATH --tool-dir PATH` locally substitutes an SDK checkout for the
released crate, compiles this project to `wasm32-unknown-unknown`, generates canonical CBOR, and
passes the unsigned module through the authoritative packer. The committed dependency remains an
exact public version; no Maverick path enters this repository.
