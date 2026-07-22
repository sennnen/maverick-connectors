#!/usr/bin/env python3
"""Validate the public-SDK connector workspace and deterministic packages."""

import argparse
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

SCHEMA_HASHES = {
    "abi": "b901e5a701e7af5794b74ff5beb05512a1e6fa0e3e76cc7c97dc72f8b66d2ea8",
    "fixtures": "1daaa3a4ea07e1c130461c61fc9a0e0d8433db60ac56f8b9bbc1073ba9cbf1ff",
    "manifest": "4ebeb126d4c17eeaccdab69320cb6d085d3b060a3d413c1e3bc8c8362ec7912b",
    "signature": "be8508dcc5fb1089828ddb7beb9fdcd5303dfaa8a95bf5c4c52f21cd5751587e",
}

def workspace_checks(root: Path) -> list[str]:
    problems: list[str] = []
    for legacy in (root / "whoop4", root / "whoop5"):
        if legacy.is_dir() and any(legacy.iterdir()):
            problems.append(f"legacy manifest folder remains: {legacy.name}")
    workspace = root / "Cargo.toml"
    template = root / "connectors" / "template" / "Cargo.toml"
    if not workspace.is_file() or not template.is_file():
        return ["public SDK-consumer workspace or template is missing"]
    protocol = root / "crates" / "whoop-protocol"
    protocol_manifest = protocol / "Cargo.toml"
    protocol_source = protocol / "src" / "lib.rs"
    if not protocol_manifest.is_file() or not protocol_source.is_file():
        problems.append("shared WHOOP protocol crate is missing")
    else:
        manifest_text = protocol_manifest.read_text()
        source_text = protocol_source.read_text()
        if "[dependencies]" in manifest_text:
            problems.append("shared WHOOP protocol crate must remain dependency-free")
        if "#![no_std]" not in source_text:
            problems.append("shared WHOOP protocol crate must remain no_std")
        forbidden = [
            "btleplug",
            "tokio",
            "rusqlite",
            "sqlite",
            "std::fs",
            "std::net",
            "std::process",
            "mav-connector-sdk",
            "mav-model",
            "mav-frame",
        ]
        for token in forbidden:
            if token in source_text or token in manifest_text:
                problems.append(f"shared WHOOP protocol crate contains forbidden boundary {token!r}")
    registry = root / "registry" / "schema-v1.json"
    if not registry.is_file():
        problems.append("ABI v1 schema registry is missing")
    else:
        try:
            schema_registry = json.loads(registry.read_text())
        except json.JSONDecodeError:
            problems.append("ABI v1 schema registry is invalid JSON")
        else:
            if schema_registry.get("schemas") != SCHEMA_HASHES:
                problems.append("ABI v1 schema registry hashes differ from frozen vectors")
    registry_paths = {
        "unsigned": root / "registry" / "index-v1.unsigned.json",
        "signed": root / "registry" / "index-v1.json",
        "root": root / "registry" / "root-v1.json",
        "schema": root / "registry" / "index-schema-v1.json",
    }
    if any(not path.is_file() for path in registry_paths.values()):
        problems.append("signed connector registry vector or schema is missing")
    else:
        try:
            unsigned_index = json.loads(registry_paths["unsigned"].read_text())
            signed_bytes = registry_paths["signed"].read_bytes()
            signed_index = json.loads(signed_bytes)
            registry_root = json.loads(registry_paths["root"].read_text())
            json.loads(registry_paths["schema"].read_text())
        except json.JSONDecodeError:
            problems.append("signed connector registry vector is invalid JSON")
        else:
            canonical = json.dumps(signed_index, separators=(",", ":")).encode()
            if canonical != signed_bytes:
                problems.append("signed connector registry vector is not canonical compact JSON")
            if signed_index.get("index") != unsigned_index:
                problems.append("signed connector registry payload differs from unsigned vector")
            signature = signed_index.get("signature", {})
            if signature.get("algorithm") != "Ed25519":
                problems.append("signed connector registry algorithm differs")
            if signature.get("key_id") != registry_root.get("key_id"):
                problems.append("signed connector registry root key id differs")
            if hashlib.sha256(signed_bytes).hexdigest() != registry_root.get("signed_index_sha256"):
                problems.append("signed connector registry digest differs from frozen vector")
            if unsigned_index.get("schema") != "mavconn-registry-index/v1":
                problems.append("unsigned connector registry schema differs")
            package_digests = {
                json.loads(path.read_text())["artifact_sha256"]
                for path in sorted(root.glob("connectors/*/package-test.json"))
            }
            entry_digests = {
                entry.get("artifact_sha256") for entry in unsigned_index.get("entries", [])
            }
            if entry_digests != package_digests:
                problems.append("signed registry entries differ from packaged connector digests")
    template_text = template.read_text()
    if 'mav-connector-sdk = "=0.1.1"' not in template_text:
        problems.append("template must pin released mav-connector-sdk =0.1.1")
    for manifest in root.glob("**/Cargo.toml"):
        for line in manifest.read_text().splitlines():
            if "mav-connector" in line and "path" in line:
                problems.append(f"{manifest}: connector dependency uses a path")
    publisher_keys: dict[str, str] = {}
    for config_path in sorted(root.glob("connectors/*/package-test.json")):
        try:
            config = json.loads(config_path.read_text())
        except json.JSONDecodeError:
            problems.append(f"{config_path}: invalid package-test JSON")
            continue
        required = {
            "schema",
            "package",
            "wasm",
            "publisher_key_id",
            "public_key_hex",
            "signed_sha256",
            "signature_hex",
            "artifact_sha256",
        }
        if set(config) != required or config.get("schema") != "mavconn-package-test/v1":
            problems.append(f"{config_path}: package-test schema or fields differ")
            continue
        for field, length in [
            ("public_key_hex", 64),
            ("signed_sha256", 64),
            ("signature_hex", 128),
            ("artifact_sha256", 64),
        ]:
            value = config[field]
            if len(value) != length or any(character not in "0123456789abcdef" for character in value):
                problems.append(f"{config_path}: {field} is not canonical lowercase hex")
        publisher_key_id = config["publisher_key_id"]
        public_key_hex = config["public_key_hex"]
        prior_key = publisher_keys.setdefault(publisher_key_id, public_key_hex)
        if prior_key != public_key_hex:
            problems.append(
                f"{config_path}: publisher key id {publisher_key_id!r} maps to multiple public keys"
            )
        parity_path = config_path.parent / "parity-v1.json"
        if not parity_path.is_file():
            problems.append(f"{parity_path}: packaged parity report is missing")
        else:
            try:
                parity = json.loads(parity_path.read_text())
            except json.JSONDecodeError:
                problems.append(f"{parity_path}: invalid parity JSON")
            else:
                if parity.get("schema") != "mavconn-parity/v1":
                    problems.append(f"{parity_path}: parity schema differs")
                if parity.get("artifact_sha256") != config["artifact_sha256"]:
                    problems.append(f"{parity_path}: artifact hash differs from package config")
                if not parity.get("fixtures"):
                    problems.append(f"{parity_path}: parity fixtures are empty")
    forbidden_suffixes = {".jks", ".p12", ".pfx", ".key", ".pem"}
    for path in root.rglob("*"):
        if path.is_file() and path.suffix.lower() in forbidden_suffixes:
            problems.append(f"private-key-shaped file is forbidden: {path}")
    return problems


def run(command: list[str], root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=root, check=True, text=True, capture_output=True)


def deep_validate(root: Path, sdk_path: Path, tool_dir: Path) -> None:
    patch = f'patch.crates-io.mav-connector-sdk.path="{sdk_path}"'
    resolver = ["--offline", "--config", patch]
    run(["cargo", "fmt", "--check"], root)
    run(
        ["cargo", "clippy"]
        + resolver
        + ["--workspace", "--all-targets", "--", "-D", "warnings"],
        root,
    )
    run(["cargo", "test"] + resolver + ["--workspace"], root)
    run(
        ["cargo", "build"]
        + resolver
        + [
            "--workspace",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
            "--lib",
        ],
        root,
    )
    release = root / "target/wasm32-unknown-unknown/release"
    package_configs = [
        (path, json.loads(path.read_text()))
        for path in sorted(root.glob("connectors/*/package-test.json"))
    ]
    wasm_paths = [release / "mav_connector_template.wasm"] + [
        release / config["wasm"] for _, config in package_configs
    ]
    first_wasm_hashes = {
        path: hashlib.sha256(path.read_bytes()).digest() for path in wasm_paths
    }
    run(
        ["cargo", "build"]
        + resolver
        + [
            "--workspace",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
            "--lib",
        ],
        root,
    )
    for path, first_hash in first_wasm_hashes.items():
        if hashlib.sha256(path.read_bytes()).digest() != first_hash:
            raise RuntimeError(f"repeated Wasm build is not deterministic: {path.name}")
    with tempfile.TemporaryDirectory(prefix="mavconn-p3-") as temporary:
        temp = Path(temporary)
        registry_root = json.loads((root / "registry/root-v1.json").read_text())
        signed_registry = json.loads((root / "registry/index-v1.json").read_text())
        registry_tool = tool_dir / "mavconn-registry"
        signing_digest = run(
            [str(registry_tool), "prepare", str(root / "registry/index-v1.unsigned.json")],
            root,
        ).stdout.strip()
        if signing_digest != registry_root["signing_digest_hex"]:
            raise RuntimeError("registry signing digest differs from frozen vector")
        generated_registry = temp / "index-v1.json"
        run(
            [
                str(registry_tool), "finalize",
                str(root / "registry/index-v1.unsigned.json"),
                registry_root["key_id"], signed_registry["signature"]["signature"],
                registry_root["public_key_hex"], str(generated_registry),
            ],
            root,
        )
        if generated_registry.read_bytes() != (root / "registry/index-v1.json").read_bytes():
            raise RuntimeError("registry finalization is not byte-deterministic")
        run(
            [
                str(registry_tool), "verify", str(generated_registry),
                registry_root["registry_id"], registry_root["key_id"],
                registry_root["public_key_hex"], "1",
            ],
            root,
        )
        run(
            ["cargo", "run"]
            + resolver
            + [
                "-p",
                "mav-connector-template",
                "--bin",
                "metadata",
                "--",
                str(temp),
            ],
            root,
        )
        unsigned_a = temp / "first.wasm"
        unsigned_b = temp / "second.wasm"
        pack = tool_dir / "mavconn-pack"
        arguments = [
            "digest",
            str(release / "mav_connector_template.wasm"),
            str(temp / "manifest.cbor"),
            str(temp / "abi.cbor"),
            str(temp / "fixtures.cbor"),
        ]
        first = run([str(pack)] + arguments + [str(unsigned_a)], root)
        second = run([str(pack)] + arguments + [str(unsigned_b)], root)
        if first.stdout != second.stdout or unsigned_a.read_bytes() != unsigned_b.read_bytes():
            raise RuntimeError("repeated unsigned packaging is not deterministic")
        for config_path, config in package_configs:
            package_temp = temp / config["package"]
            package_temp.mkdir()
            run(
                ["cargo", "run"]
                + resolver
                + [
                    "-p",
                    config["package"],
                    "--bin",
                    "metadata",
                    "--",
                    str(package_temp),
                ],
                root,
            )
            unsigned = package_temp / "unsigned.wasm"
            digest = run(
                [
                    str(pack),
                    "digest",
                    str(release / config["wasm"]),
                    str(package_temp / "manifest.cbor"),
                    str(package_temp / "abi.cbor"),
                    str(package_temp / "fixtures.cbor"),
                    str(unsigned),
                ],
                root,
            ).stdout.strip()
            if digest != config["signed_sha256"]:
                raise RuntimeError(f"{config_path}: signed digest differs")
            artifact = package_temp / "connector.mavconn"
            run(
                [
                    str(pack),
                    "finalize",
                    str(unsigned),
                    config["publisher_key_id"],
                    config["public_key_hex"],
                    config["signature_hex"],
                    str(artifact),
                ],
                root,
            )
            artifact_hash = hashlib.sha256(artifact.read_bytes()).hexdigest()
            if artifact_hash != config["artifact_sha256"]:
                raise RuntimeError(f"{config_path}: final artifact digest differs")
            run(
                [str(tool_dir / "mavconn-validate"), str(artifact), config["public_key_hex"]],
                root,
            )
            registry_root = json.loads((root / "registry/root-v1.json").read_text())
            connector_id = json.loads((config_path.parent / "parity-v1.json").read_text())["connector_id"]
            run(
                [
                    str(tool_dir / "mavconn-registry"), "verify-artifact",
                    str(root / "registry/index-v1.json"), registry_root["registry_id"],
                    registry_root["key_id"], registry_root["public_key_hex"], "1",
                    connector_id, "1.0.0", str(artifact),
                ],
                root,
            )
            generated_report = package_temp / "parity-v1.json"
            run(
                [
                    str(tool_dir / "mavconn-test"),
                    str(artifact),
                    config["public_key_hex"],
                    "--report",
                    str(generated_report),
                ],
                root,
            )
            expected_report = config_path.parent / "parity-v1.json"
            if generated_report.read_bytes() != expected_report.read_bytes():
                raise RuntimeError(f"{expected_report}: generated parity report differs")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sdk-path", type=Path)
    parser.add_argument("--tool-dir", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    failures = workspace_checks(root)

    if failures:
        for f in failures:
            print(f)
        return 1
    if (args.sdk_path is None) != (args.tool_dir is None):
        print("--sdk-path and --tool-dir must be supplied together", file=sys.stderr)
        return 1
    if args.sdk_path is not None and args.tool_dir is not None:
        try:
            deep_validate(root, args.sdk_path.resolve(), args.tool_dir.resolve())
        except (subprocess.CalledProcessError, OSError, RuntimeError) as error:
            print(f"deep validation failed: {error}", file=sys.stderr)
            if isinstance(error, subprocess.CalledProcessError):
                print(error.stdout, file=sys.stderr)
                print(error.stderr, file=sys.stderr)
            return 1
        print("validate: SDK template build and deterministic unsigned package ok")
    print("validate: SDK workspace and packaged artifacts ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
