# Connector repository architecture

This repository owns connector developer experience, device-specific source, fixtures, packaging,
and releases. `sennnen/maverick` owns host ABI, runtime, installer, trust enforcement, generic BLE
execution, persistence, analytics, FFI, and apps.

## Dependency direction

```text
mav-connector-sdk (public, device-neutral)
          ^
          |
connector-local pure libraries
          ^
          |
whoop4 project       whoop5 project       third-party projects
          \             |                 /
           +---- wasm32-unknown-unknown --+
                           |
                     pack + sign
                           |
                    one .mavconn
```

Connector projects depend only on released public SDK surfaces and portable crates admitted by the
toolchain. No path dependency reaches a Maverick core crate. Shared device-family source stays here,
not in Maverick. SDK source may initially be developed in Maverick to freeze host/guest vectors, but
its consumable release and compatibility policy are public and device-neutral.

## Artifact ownership

Source projects own manifest declarations, protocol state, fixtures, and expected outputs. Packer
owns deterministic custom-section order/encoding and signature construction. Validator uses the
same parser, schemas, limits, interpreter adapter, and fixture harness as Maverick; a second shallow
definition cannot become authoritative.

Release metadata records connector id/version, artifact SHA-256, publisher key id, ABI/core ranges,
toolchain/SDK versions, source commit, fixture-set hash, and signature. Built artifacts and public
keys are allowed; private signing material is not.

## WHOOP split

WHOOP 4.0 and WHOOP 5.0/MG ship separately. Shared pure code is allowed only for evidenced common
wire behaviour. Advertisement, bond order, hello, UUID mapping, record dispatch, firmware quirks,
and state transitions remain generation-local where evidence differs. A compiler enum shared by the
two projects is not permission to merge their product artifacts.

`tanarchytan/whoop-rs` is a read-only primary reference during migration. Portable candidates are
its sans-IO protocol/framing/record/offload concepts. Desktop `btleplug`, tokio client orchestration,
CLI, SQLite, FFI, and analytics do not enter connector artifacts. Behaviour is adjudicated against
Maverick fixtures and traceable captures, not copied blindly.

## Registry boundary

Registry contains signed discovery/update metadata and digest-addressed artifact locations. It does
not grant runtime capabilities and cannot replace publisher signatures. Direct URL/local import
remains supported. Registry signing keys, connector publisher keys, and mobile app release keys are
three distinct roles.
