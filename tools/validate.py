#!/usr/bin/env python3
"""Structural validation of every connector manifest in this repository.

This is the shallow check that needs nothing but a Python interpreter: valid JSON, the required
keys present, the schema and wire format recognised, and the enable sequence internally consistent.
The authoritative deep validation is the core's `mav-codec` schema, run locally with the core
checked out; this harness exists so the connectors repository's own CI can fail on a broken manifest
without needing the private core repo.
"""

import json
import sys
from pathlib import Path

REQUIRED_TOP = ["schema", "identity", "gatt", "frame", "packets", "capabilities"]
WIRE_FORMATS = {"gen4", "gen5"}
SCHEMA = "connector-manifest/v1"


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
    # the core's mav-codec (for example "r20_k18"), not a manifest layout. Historical records route
    # through those decoders, not through the layout DSL, so the shallow check here only confirms the
    # value is a non-empty string; the core's `validate_manifests` example checks it against the
    # actual admitted-decoder list.
    for version, decoder in m.get("record_versions", {}).items():
        if not isinstance(decoder, str) or not decoder:
            problems.append(
                f"{manifest_path}: record_versions[{version}] must name a record decoder"
            )
    return problems


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    manifests = sorted(root.glob("*/manifest.json"))
    if not manifests:
        print("no manifests found", file=sys.stderr)
        return 1

    failures: list[str] = []
    for manifest in manifests:
        failures.extend(check(manifest))

    if failures:
        for f in failures:
            print(f)
        return 1
    print(f"validate: {len(manifests)} manifest(s) ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
