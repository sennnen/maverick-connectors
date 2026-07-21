#!/usr/bin/env python3
"""Prepare or finalize one digest-addressed connector registry publication.

Private keys never enter this process. `prepare` writes the updated unsigned index and prints the
32-byte digest for an external Ed25519 signer. `finalize` accepts only its public key and signature.
"""

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

MAX_ARTIFACT_BYTES = 4 * 1024 * 1024


def run(command: list[str]) -> str:
    completed = subprocess.run(command, check=True, text=True, capture_output=True)
    return completed.stdout.strip()


def prepare(args: argparse.Namespace) -> None:
    artifact = args.artifact.read_bytes()
    if not artifact or len(artifact) > MAX_ARTIFACT_BYTES:
        raise ValueError("artifact must contain 1..4194304 bytes")
    digest = hashlib.sha256(artifact).hexdigest()
    expected_url = args.artifact_url
    if not expected_url.startswith("https://") or digest not in expected_url:
        raise ValueError("artifact URL must be HTTPS and digest-addressed")
    index = json.loads(args.index.read_text())
    if index.get("schema") != "mavconn-registry-index/v1":
        raise ValueError("unsigned index schema differs")
    entry = {
        "connector_id": args.connector_id,
        "version": args.version,
        "artifact_sha256": digest,
        "artifact_url": expected_url,
        "artifact_size": len(artifact),
        "publisher_key_id": args.publisher_key_id,
        "abi": {"major": 1, "min_minor": 0, "max_minor": 0},
        "core": {"min_version": args.core_min_version, "max_version": None},
        "channel": args.channel,
        "supersedes": args.supersedes,
        "revoked": False,
    }
    identity = (args.connector_id, args.version, args.channel)
    entries = [
        item for item in index["entries"]
        if (item["connector_id"], item["version"], item["channel"]) != identity
    ]
    entries.append(entry)
    index["entries"] = sorted(entries, key=lambda item: (
        item["connector_id"], item["version"], item["channel"]
    ))
    args.output_index.write_text(json.dumps(index, indent=2) + "\n")
    args.release_dir.mkdir(parents=True, exist_ok=True)
    release = args.release_dir / f"sha256-{digest}.mavconn"
    if release.exists() and release.read_bytes() != artifact:
        raise ValueError(f"digest-addressed release collision: {release}")
    if not release.exists():
        shutil.copyfile(args.artifact, release)
    print(run([str(args.registry_tool), "prepare", str(args.output_index)]))


def finalize(args: argparse.Namespace) -> None:
    run([
        str(args.registry_tool), "finalize", str(args.index), args.root_key_id,
        args.signature_hex, args.public_key_hex, str(args.output),
    ])
    print(hashlib.sha256(args.output.read_bytes()).hexdigest())


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    commands = result.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare")
    prepare_parser.add_argument("--artifact", type=Path, required=True)
    prepare_parser.add_argument("--connector-id", required=True)
    prepare_parser.add_argument("--version", required=True)
    prepare_parser.add_argument("--publisher-key-id", required=True)
    prepare_parser.add_argument("--artifact-url", required=True)
    prepare_parser.add_argument("--channel", default="stable")
    prepare_parser.add_argument("--supersedes")
    prepare_parser.add_argument("--core-min-version", default="0.1.0")
    prepare_parser.add_argument("--index", type=Path, required=True)
    prepare_parser.add_argument("--output-index", type=Path, required=True)
    prepare_parser.add_argument("--release-dir", type=Path, required=True)
    prepare_parser.add_argument("--registry-tool", type=Path, required=True)
    prepare_parser.set_defaults(action=prepare)
    finalize_parser = commands.add_parser("finalize")
    finalize_parser.add_argument("--index", type=Path, required=True)
    finalize_parser.add_argument("--root-key-id", required=True)
    finalize_parser.add_argument("--signature-hex", required=True)
    finalize_parser.add_argument("--public-key-hex", required=True)
    finalize_parser.add_argument("--output", type=Path, required=True)
    finalize_parser.add_argument("--registry-tool", type=Path, required=True)
    finalize_parser.set_defaults(action=finalize)
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        args.action(args)
    except (OSError, ValueError, KeyError, json.JSONDecodeError, subprocess.CalledProcessError) as error:
        print(f"publish failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
