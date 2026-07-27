# Maverick connectors

Source projects and packaged releases for Maverick's independently installable device connectors.
Target format is one signed `.mavconn` file: a valid WebAssembly module containing deterministic
manifest, ABI, fixture, and signature custom sections. Identical bytes run through Maverick's shared
Rust interpreter on iOS, Android, replay, and tests.

This repository is private. It contains the public-SDK consumer workspace, deterministic
device-neutral template, schema registry, local deep validation path, and the two packaged device
connector projects. Runtime and authoritative tools live in Maverick. The executable migration is
[docs/migration.md](docs/migration.md); target source/release architecture is
[docs/architecture.md](docs/architecture.md). Core contracts and security model live in
[`sennnen/maverick`](https://github.com/sennnen/maverick/blob/main/docs/connectors.md), with the
decision in [ADR-017](https://github.com/sennnen/maverick/blob/main/docs/adr/ADR-017.md).

## Product invariants

- Anyone can author and publish a connector without editing or rebuilding Maverick.
- One artifact installs from URL, local file, native share/open flow, or registry on both platforms.
- Connector code uses the public SDK and receives only normalized events; it returns bounded actions.
- No connector gets filesystem, network, native BLE, process, thread, clock, or random access.
- Metadata, ABI compatibility, publisher signature, revocation, limits, and embedded tests pass
  before installation.
- WHOOP 4.0 and WHOOP 5.0/MG are separate artifacts using the same public path as third parties.
- Exactly one artifact ships inside the app — Generic HR Monitor — and it installs through that same
  public path, inspection and approval token included. A published standard is not a device driver;
  bundling the latter is what the connector architecture exists to avoid.

## Current contents

- `connectors/generic-hr/` — the Bluetooth SIG Heart Rate Service connector, "Generic HR Monitor";
- `connectors/template/` — exact-version public SDK consumer, native test, Wasm exports, metadata;
- `connectors/whoop4/` — signed-test WHOOP 4.0 connector with fourteen parity-profiled fixtures;
- `connectors/whoop5/` — signed-test WHOOP 5.0/MG connector with twelve parity-profiled fixtures;
- `crates/ble-sig/` — no-std adopted Bluetooth SIG profiles, decoded once for every connector;
- `crates/whoop-protocol/` — no-std pure framing, control, safe-offload, and record-routing reference;
- `registry/schema-v1.json` — frozen ABI v1 schema hashes used by external authors;
- `registry/index-schema-v1.json` — signed discovery, rotation, and revocation schema;
- `registry/index-v1.json` — deterministic signed test index with public root metadata;
- `tools/validate.py` — standalone workspace/package checks plus opt-in deep SDK/tool validation.
- `tools/publish.py` — keyless two-phase digest-addressed registry publication.

## Target contents

```text
connectors/
  generic-hr/               Bluetooth SIG Heart Rate Service, the one connector the app ships with
  template/                 public-SDK consumer and metadata example
  whoop4/                   standalone SDK project + fixtures
  whoop5/                   standalone SDK project + fixtures
crates/
  ble-sig/                  adopted SIG profiles, shared by every connector that speaks one
  whoop-protocol/           connector-local shared pure protocol code
tools/                      local validation and future publish wrappers
registry/                   signed metadata/index fixtures; never private signing keys
releases/                   digest-addressed .mavconn outputs or release metadata
```

WC-P3 froze the workspace/template layout. WC-P8 froze WHOOP-local pure protocol source and its
[evidence matrix](docs/whoop-protocol-matrix.md); WC-P9/P10 delivered the generation connectors,
WC-P11 froze parity, WC-P12 removed the legacy JSON path, and WC-P15 added the signed registry and
[keyless publishing flow](docs/publishing.md).

## Security

Connector publisher signing is separate from Android/iOS application signing and registry signing.
No *production* private key belongs in this repository. Packer accepts only public-key and signature
bytes produced outside the process, then verifies its own output. The local
`maverick-signing/maverick-release.jks` is an Android release asset and is not a connector key.

The one deliberate exception is the **sandbox test-fixture** key. The `.mavconn` files, their
`package-test.json`/`parity-v1.json` reports, and the development registry are signed by a committed
Ed25519 test seed under publisher `maverick-whoop-live-test` — held by `tools/testsign.py`, exactly
as the Maverick core crates commit `[N; 32]` seeds for their own trust tests. It is DEVELOPMENT scope
only; every production trust policy refuses it, and it never signs a release artifact. This lets
`tools/regenerate.py --check` stay a keyless freshness gate that anyone can reproduce, instead of a
frozen state nobody can refresh once the original external signer walks away.

---

Independent and unofficial. Not affiliated with, endorsed by, or sponsored by WHOOP, Inc. “WHOOP”
names hardware these connectors interoperate with.
