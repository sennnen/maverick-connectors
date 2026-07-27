#!/usr/bin/env python3
"""Regenerate every TEST fixture and the development registry, signing with committed TEST seeds.

**This signs TEST material only.** The two seeds below are committed deliberately. The fixtures they
sign live under publisher `maverick-whoop-live-test` and registry `dev.maverick.connectors` — a sandbox
trust domain the apps trust only in debug/test builds, exactly like the `[N; 32]` seeds the Maverick
core crates commit for their own trust tests. Production connectors are signed by an **external**
Ed25519 holder through `tools/publish.py`, which never sees a private key. Never point these seeds at
a production artifact.

Why this exists: the original fixtures were signed by an external throwaway key that was then
discarded. When connector source legitimately changed, the frozen digests in `package-test.json`,
`parity-v1.json`, and the registry could not be refreshed — deep validation went red with no
committed way to make it green again. A deterministic, committed test signer removes that dead end.
The division of labour is:

    tools/regenerate.py --check     keyless CI freshness gate: is what is committed still fresh?
    tools/testsign.py               keyed refresh a human runs when connector source changes

Every signature this script produces is verified with the Rust `ed25519-dalek` tools before it is
written, so a bug in the pure-Python signer cannot land a bad fixture.

    python3 tools/testsign.py \
        --sdk-path ../maverick/core/crates/mav-connector-sdk \
        --tool-dir ../maverick/core/target/release \
        --maverick-root ../maverick        # optional: also re-vendor the fixtures into maverick
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _ed25519
from regenerate import ROOT, build_wasm, package_configs, resolver, run, signed_digest

# TEST-ONLY seeds. See the module docstring: never production keys.
PUBLISHER_SEED = bytes([151]) * 32  # key_id "maverick-whoop-live-test"
REGISTRY_SEED = bytes([152]) * 32  # key_id "registry-root-v1"

ARTIFACT_URL = (
    "https://github.com/sennnen/maverick-connectors/releases/download/registry-v1/sha256-{}.mavconn"
)
# package name -> the basename maverick vendors it under in fixtures/connectors/
MAVERICK_FIXTURE = {
    "mav-connector-generic-hr": "generic_hr",
    "mav-connector-whoop4": "whoop4",
    "mav-connector-whoop5": "whoop5",
}


def regenerate(sdk_path: Path, tool_dir: Path, maverick_root: Path | None) -> int:
    publisher_pub = _ed25519.public_key(PUBLISHER_SEED).hex()
    registry_pub = _ed25519.public_key(REGISTRY_SEED).hex()
    release = build_wasm(sdk_path)

    artifacts: dict[str, tuple[str, int]] = {}  # connector_id -> (artifact_sha256, size)
    work = ROOT / "target/testsign"
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)

    for config_path, config in package_configs():
        package = config["package"]
        workdir = work / package
        workdir.mkdir()
        digest, unsigned = signed_digest(config, release, tool_dir, sdk_path, workdir)
        signature = _ed25519.sign(bytes.fromhex(digest), PUBLISHER_SEED).hex()

        artifact = workdir / "connector.mavconn"
        run([
            str(tool_dir / "mavconn-pack"), "finalize", str(unsigned),
            config["publisher_key_id"], publisher_pub, signature, str(artifact),
        ])
        # The Rust verifier is the real check that the pure-Python signature is sound.
        run([str(tool_dir / "mavconn-validate"), str(artifact), publisher_pub])
        artifact_bytes = artifact.read_bytes()
        artifact_sha256 = hashlib.sha256(artifact_bytes).hexdigest()

        config.update({
            "public_key_hex": publisher_pub,
            "signed_sha256": digest,
            "signature_hex": signature,
            "artifact_sha256": artifact_sha256,
        })
        config_path.write_text(json.dumps(config, indent=2) + "\n")

        report = config_path.parent / "parity-v1.json"
        run([
            str(tool_dir / "mavconn-test"), str(artifact), publisher_pub, "--report", str(report),
        ])

        report_data = json.loads(report.read_text())
        # The registry entry must carry the artifact's own manifest version, or the runtime's
        # verify_artifact rejects it (MAV-11059). Read it back rather than trusting a frozen literal.
        artifacts[report_data["connector_id"]] = (
            artifact_sha256, len(artifact_bytes), report_data["connector_version"]
        )

        if maverick_root is not None:
            name = MAVERICK_FIXTURE[package]
            dest = maverick_root / "fixtures/connectors"
            shutil.copyfile(artifact, dest / f"{name}_v1.mavconn")
            shutil.copyfile(report, dest / f"{name}_parity_v1.expected.json")

    rewrite_registry(tool_dir, registry_pub, artifacts, work)
    print(f"testsign: publisher public key {publisher_pub}")
    print(f"testsign: registry  public key {registry_pub}")
    print("testsign: regenerated package-test.json, parity-v1.json, and the registry")
    if maverick_root is not None:
        print(f"testsign: re-vendored fixtures into {maverick_root}/fixtures/connectors")
    return 0


def rewrite_registry(
    tool_dir: Path, registry_pub: str, artifacts: dict[str, tuple[str, int, str]], work: Path
) -> None:
    unsigned_path = ROOT / "registry/index-v1.unsigned.json"
    index = json.loads(unsigned_path.read_text())
    for entry in index["entries"]:
        sha256, size, version = artifacts[entry["connector_id"]]
        entry["version"] = version
        entry["artifact_sha256"] = sha256
        entry["artifact_url"] = ARTIFACT_URL.format(sha256)
        entry["artifact_size"] = size
    unsigned_path.write_text(json.dumps(index, indent=2) + "\n")

    registry_tool = tool_dir / "mavconn-registry"
    signing_digest = run([str(registry_tool), "prepare", str(unsigned_path)]).stdout.strip()
    signature = _ed25519.sign(bytes.fromhex(signing_digest), REGISTRY_SEED).hex()

    signed_path = ROOT / "registry/index-v1.json"
    root = json.loads((ROOT / "registry/root-v1.json").read_text())
    run([
        str(registry_tool), "finalize", str(unsigned_path),
        root["key_id"], signature, registry_pub, str(signed_path),
    ])
    signed_index_sha256 = hashlib.sha256(signed_path.read_bytes()).hexdigest()

    root.update({
        "public_key_hex": registry_pub,
        "signing_digest_hex": signing_digest,
        "signed_index_sha256": signed_index_sha256,
    })
    (ROOT / "registry/root-v1.json").write_text(json.dumps(root, indent=2) + "\n")

    # Prove the freshly signed index verifies under the rotated key.
    run([
        str(registry_tool), "verify", str(signed_path),
        root["registry_id"], root["key_id"], registry_pub, "1",
    ])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk-path", type=Path, required=True)
    parser.add_argument("--tool-dir", type=Path, required=True)
    parser.add_argument("--maverick-root", type=Path, default=None)
    args = parser.parse_args()
    maverick_root = args.maverick_root.resolve() if args.maverick_root else None
    return regenerate(args.sdk_path.resolve(), args.tool_dir.resolve(), maverick_root)


if __name__ == "__main__":
    raise SystemExit(main())
