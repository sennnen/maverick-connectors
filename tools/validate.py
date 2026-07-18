#!/usr/bin/env python3
"""Validate legacy manifests and the public-SDK consumer workspace.

This is the shallow check that needs nothing but a Python interpreter: valid JSON, the required
keys present, the schema and wire format recognised, and the enable sequence internally consistent.
The authoritative deep validation is the core's `mav-codec` schema, run locally with the core
checked out; this harness exists so the connectors repository's own CI can fail on a broken manifest
without needing the private core repo. WC-P3 deep mode accepts an explicit SDK checkout and tool
directory for local pre-publication testing; neither path is written into repository manifests.
"""

import argparse
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

REQUIRED_TOP = ["schema", "identity", "gatt", "frame", "packets", "capabilities"]
WIRE_FORMATS = {"gen4", "gen5"}
SCHEMA = "connector-manifest/v1"
SCHEMA_HASHES = {
    "abi": "b901e5a701e7af5794b74ff5beb05512a1e6fa0e3e76cc7c97dc72f8b66d2ea8",
    "fixtures": "1daaa3a4ea07e1c130461c61fc9a0e0d8433db60ac56f8b9bbc1073ba9cbf1ff",
    "manifest": "4ebeb126d4c17eeaccdab69320cb6d085d3b060a3d413c1e3bc8c8362ec7912b",
    "signature": "be8508dcc5fb1089828ddb7beb9fdcd5303dfaa8a95bf5c4c52f21cd5751587e",
}


def check(manifest_path: Path) -> list[str]:
    problems: list[str] = []
    try:
        m = json.loads(manifest_path.read_text())
    except json.JSONDecodeError as e:
        return [f"{manifest_path}: invalid JSON: {e}"]

    for key in REQUIRED_TOP:
        if key not in m:
            problems.append(f"{manifest_path}: missing required key '{key}'")
    if problems:
        return problems

    if m["schema"] != SCHEMA:
        problems.append(f"{manifest_path}: schema is {m['schema']!r}, want {SCHEMA!r}")
    if m["frame"].get("wire_format") not in WIRE_FORMATS:
        problems.append(f"{manifest_path}: wire_format must be one of {sorted(WIRE_FORMATS)}")
    if not m["identity"].get("models"):
        problems.append(f"{manifest_path}: identity.models must not be empty")
    if not m["packets"]:
        problems.append(f"{manifest_path}: packets map must not be empty")
    if not m["capabilities"]:
        problems.append(f"{manifest_path}: capabilities must not be empty")

    command_names = {c.get("name") for c in m.get("commands", [])}
    seq = m.get("enable_sequence")
    if seq is not None:
        if seq.get("command") not in command_names:
            problems.append(
                f"{manifest_path}: enable_sequence.command "
                f"{seq.get('command')!r} names no command"
            )
        if seq.get("name_field_bytes", 0) >= seq.get("payload_bytes", 0):
            problems.append(
                f"{manifest_path}: enable_sequence leaves no room for a value byte"
            )

    # record_versions maps a version byte to an admitted record-decoder id -- a reviewed module in
    # the device's codec crate (core/connectors/mav-connector-<family>, ADR-016; for example
    # "r20_k18"), not a manifest layout. Historical records route through those decoders, not
    # through the layout DSL, so the shallow check here only confirms the value is a non-empty
    # string; the core checks it against what the named codec actually admits.
    for version, decoder in m.get("record_versions", {}).items():
        if not isinstance(decoder, str) or not decoder:
            problems.append(
                f"{manifest_path}: record_versions[{version}] must name a record decoder"
            )

    # event_vocabulary names an admitted event-vocabulary module in the device's codec crate (for
    # example "whoop"); the shallow check only confirms the shape, the core checks it against what
    # the named codec actually admits.
    vocabulary = m.get("event_vocabulary")
    if vocabulary is not None and (not isinstance(vocabulary, str) or not vocabulary):
        problems.append(f"{manifest_path}: event_vocabulary must name a vocabulary")

    # Anything routed through a device codec requires naming one (ADR-016).
    if (m.get("record_versions") or vocabulary is not None) and not m.get("codec"):
        problems.append(
            f"{manifest_path}: record_versions/event_vocabulary require a codec id"
        )
    return problems


def workspace_checks(root: Path) -> list[str]:
    problems: list[str] = []
    workspace = root / "Cargo.toml"
    template = root / "connectors" / "template" / "Cargo.toml"
    if not workspace.is_file() or not template.is_file():
        return ["public SDK-consumer workspace or template is missing"]
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
    template_text = template.read_text()
    if 'mav-connector-sdk = "=0.1.0"' not in template_text:
        problems.append("template must pin released mav-connector-sdk =0.1.0")
    for manifest in root.glob("**/Cargo.toml"):
        for line in manifest.read_text().splitlines():
            if "mav-connector" in line and "path" in line:
                problems.append(f"{manifest}: connector dependency uses a path")
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
    wasm = root / "target/wasm32-unknown-unknown/release/mav_connector_template.wasm"
    first_wasm_hash = hashlib.sha256(wasm.read_bytes()).digest()
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
    if hashlib.sha256(wasm.read_bytes()).digest() != first_wasm_hash:
        raise RuntimeError("repeated template Wasm build is not deterministic")
    with tempfile.TemporaryDirectory(prefix="mavconn-p3-") as temporary:
        temp = Path(temporary)
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
            str(wasm),
            str(temp / "manifest.cbor"),
            str(temp / "abi.cbor"),
            str(temp / "fixtures.cbor"),
        ]
        first = run([str(pack)] + arguments + [str(unsigned_a)], root)
        second = run([str(pack)] + arguments + [str(unsigned_b)], root)
        if first.stdout != second.stdout or unsigned_a.read_bytes() != unsigned_b.read_bytes():
            raise RuntimeError("repeated unsigned packaging is not deterministic")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sdk-path", type=Path)
    parser.add_argument("--tool-dir", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    manifests = sorted(root.glob("*/manifest.json"))
    if not manifests:
        print("no manifests found", file=sys.stderr)
        return 1

    failures: list[str] = []
    for manifest in manifests:
        failures.extend(check(manifest))
    failures.extend(workspace_checks(root))

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
    print(f"validate: {len(manifests)} legacy manifest(s) + workspace ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
