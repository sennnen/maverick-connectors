# Publishing connectors

Registry publication never grants execution privilege. The registry root signs deterministic
discovery metadata; each downloaded `.mavconn` must still pass its ordinary publisher signature,
platform trust, compatibility, revocation, resource, and embedded-fixture checks in Maverick core.

## Trust separation

- Use three unrelated identities: connector publisher, registry root, and app release signing.
- Keep every private key outside git, logs, shell history, CI artifacts, and this toolchain.
- A registry entry carries only a publisher key id. It cannot introduce or replace the trusted
  publisher public key.
- Publisher rotation requires a domain-separated cross-signature from the already trusted old key.
  The registry merely carries that record.
- Revocations are monotonic. A refreshed index may add them but cannot remove cached records.

## Two-phase publication

Build and externally sign the connector with `mavconn-pack` first. Then prepare a digest-addressed
release and unsigned index update:

```text
python3 tools/publish.py prepare \
  --artifact connector.mavconn \
  --connector-id org.example.device \
  --version 1.0.0 \
  --publisher-key-id example-publisher-v1 \
  --artifact-url https://example.invalid/releases/sha256-<artifact-sha256>.mavconn \
  --index registry/index-v1.unsigned.json \
  --output-index next-index.json \
  --release-dir releases \
  --registry-tool /path/to/mavconn-registry
```

The command prints the registry signing digest. Sign those exact 32 bytes with the external
Ed25519 registry root, then finalize using only public material:

```text
python3 tools/publish.py finalize \
  --index next-index.json \
  --root-key-id registry-root-v1 \
  --signature-hex <external-signature> \
  --public-key-hex <registry-public-key> \
  --output registry/index-v1.json \
  --registry-tool /path/to/mavconn-registry
```

Publish the exact digest-addressed artifact and signed compact index. Never rewrite bytes at an
existing digest URL. Increment the index revision, link `previous_index_sha256` to the full prior
signed envelope, and advance the revocation revision whenever revocation content changes. Direct
URL, file, and share imports remain supported when registry discovery is disabled.

Before upload, run `mavconn-registry verify-artifact` against the finalized index and artifact. It
checks signed entry size/digest plus connector id, version, publisher id, ABI/core ranges, and
channel against the artifact's own signed manifest. Normal client installation still repeats this
binding and the publisher signature check.
