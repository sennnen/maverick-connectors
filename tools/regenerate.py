#!/usr/bin/env python3
"""Rebuild every connector from source and compare, or prepare, its release artifacts.

Two modes, and the split matters:

    tools/regenerate.py --check     rebuild from source and fail if the packaged digests are stale
    tools/regenerate.py prepare     rebuild and write the unsigned artifacts plus their digests

`--check` is the freshness gate. It needs no key, runs on every pull request, and answers one
question: does what is committed in `package-test.json` still describe what this source compiles to?
The audit that motivated this script found the answer was no — the registry, the package configs, and
the artifacts vendored into maverick had drifted from connector source, and nothing noticed because
the deep validation path was never reached in CI.

`prepare` stops at the unsigned artifact and prints the digest an external signer must sign, exactly
as `tools/publish.py` does. **A private key never enters this process.** That is a deliberate
deviation from the packet as planned, which called for an in-run throwaway signer: a script that can
mint a signing key is a script that can sign anything it builds, and the whole point of the trust
model is that packaging and signing are separate acts by separate holders.

Finish a release with `tools/publish.py prepare` / `finalize`, which already take a signature and a
public key and refuse to take anything else.
"""

import argparse
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def run(command: list[str], cwd: Path = ROOT) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=cwd, check=True, text=True, capture_output=True)


def resolver(sdk_path: Path) -> list[str]:
    return ["--config", f'patch.crates-io.mav-connector-sdk.path="{sdk_path}"']


def package_configs() -> list[tuple[Path, dict]]:
    return [
        (path, json.loads(path.read_text()))
        for path in sorted(ROOT.glob("connectors/*/package-test.json"))
    ]


def build_wasm(sdk_path: Path) -> Path:
    """Build every connector for wasm32 twice and prove the two builds agree byte for byte."""
    build = (
        ["cargo", "build"]
        + resolver(sdk_path)
        + ["--workspace", "--target", "wasm32-unknown-unknown", "--release", "--lib"]
    )
    run(build)
    release = ROOT / "target/wasm32-unknown-unknown/release"
    first = {
        path.name: hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(release.glob("*.wasm"))
    }
    run(build)
    for path in sorted(release.glob("*.wasm")):
        if hashlib.sha256(path.read_bytes()).hexdigest() != first.get(path.name):
            raise RuntimeError(f"repeated Wasm build is not deterministic: {path.name}")
    return release


def signed_digest(
    config: dict, release: Path, tool_dir: Path, sdk_path: Path, workdir: Path
) -> tuple[str, Path]:
    """Emit the connector's metadata, pack it with its wasm, and return the digest to be signed."""
    run(
        ["cargo", "run"]
        + resolver(sdk_path)
        + ["-p", config["package"], "--bin", "metadata", "--", str(workdir)]
    )
    unsigned = workdir / "unsigned.wasm"
    digest = run(
        [
            str(tool_dir / "mavconn-pack"),
            "digest",
            str(release / config["wasm"]),
            str(workdir / "manifest.cbor"),
            str(workdir / "abi.cbor"),
            str(workdir / "fixtures.cbor"),
            str(unsigned),
        ]
    ).stdout.strip()
    return digest, unsigned


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "mode",
        nargs="?",
        default="prepare",
        choices=["prepare"],
        help="rebuild and write unsigned artifacts (the default)",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="rebuild and compare against the committed digests; write nothing",
    )
    parser.add_argument("--sdk-path", type=Path, required=True)
    parser.add_argument("--tool-dir", type=Path, required=True)
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=ROOT / "target/regenerate",
        help="where prepare writes the unsigned artifacts",
    )
    args = parser.parse_args()

    sdk_path = args.sdk_path.resolve()
    tool_dir = args.tool_dir.resolve()
    release = build_wasm(sdk_path)

    stale: list[str] = []
    prepared: list[tuple[str, str, Path]] = []
    with tempfile.TemporaryDirectory(prefix="mavconn-regen-") as temporary:
        for config_path, config in package_configs():
            workdir = Path(temporary) / config["package"]
            workdir.mkdir(parents=True)
            digest, unsigned = signed_digest(config, release, tool_dir, sdk_path, workdir)
            committed = config["signed_sha256"]
            if digest != committed:
                stale.append(
                    f"{config_path.relative_to(ROOT)}: source digest {digest}"
                    f" but package-test.json holds {committed}"
                )
            if not args.check:
                args.out_dir.mkdir(parents=True, exist_ok=True)
                target = args.out_dir / f"{config['package']}-unsigned.wasm"
                target.write_bytes(unsigned.read_bytes())
                prepared.append((config["package"], digest, target))

    if args.check:
        for line in stale:
            print(f"regenerate: {line}", file=sys.stderr)
        if stale:
            print(
                "regenerate: packaged artifacts no longer match connector source;"
                " run tools/regenerate.py prepare, sign the printed digests, and publish",
                file=sys.stderr,
            )
            return 1
        print("regenerate: packaged digests match connector source")
        return 0

    for package, digest, target in prepared:
        print(f"regenerate: {package}")
        print(f"  unsigned artifact: {target.relative_to(ROOT)}")
        print(f"  digest to sign:    {digest}")
    if stale:
        print(
            "\nregenerate: these digests differ from the committed ones, which is expected for a"
            "\nrelease. Sign them, then finalize with tools/publish.py."
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
