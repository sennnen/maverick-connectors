# Maverick connectors

Source projects and packaged releases for Maverick's independently installable device connectors.
Target format is one signed `.mavconn` file: a valid WebAssembly module containing deterministic
manifest, ABI, fixture, and signature custom sections. Identical bytes run through Maverick's shared
Rust interpreter on iOS, Android, replay, and tests.

This repository is private and currently contains legacy JSON manifests only. Runtime, SDK, packer,
and packaged connectors are planned but not implemented. The executable migration is
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

## Current contents

- `whoop4/manifest.json` — legacy WHOOP 4.0 manifest, retained as migration evidence.
- `whoop5/manifest.json` — legacy WHOOP 5.0/MG manifest, retained as migration evidence.
- `tools/validate.py` — shallow validator for those legacy manifests.

These are not installable plugin artifacts and do not prove the target architecture. WC-P12 deletes
them after both packaged connectors pass native-versus-Wasm parity and the active runtime switches.

## Future contents

```text
crates/
  mav-connector-sdk/        public ABI, macros, bounded builders, test harness
  whoop-protocol/           connector-local shared pure protocol code
connectors/
  whoop4/                   standalone SDK project + fixtures
  whoop5/                   standalone SDK project + fixtures
tools/                      pack, inspect, validate, test, publish
registry/                   signed metadata/index fixtures; never private signing keys
releases/                   digest-addressed .mavconn outputs or release metadata
```

Exact layout freezes in WC-P3/WC-P8; do not create empty scaffolding before those packets.

## Security

Connector publisher signing is separate from Android/iOS application signing and registry signing.
No private key belongs in this repository. Packer accepts an external signer/key source, emits only
public identity and signature bytes, then verifies its own output. The local
`maverick-signing/maverick-release.jks` is an Android release asset and is not a connector key.

---

Independent and unofficial. Not affiliated with, endorsed by, or sponsored by WHOOP, Inc. “WHOOP”
names hardware these connectors interoperate with.
