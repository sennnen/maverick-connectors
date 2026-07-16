# Maverick connectors

Device connectors for [Maverick](https://github.com/sennnen/maverick), the local-first wearable
platform. A connector teaches Maverick how to talk to one family of device. It is a `manifest.json`
of static facts (identity, GATT UUIDs, frame parameters, the packet map, field layouts, unit
conversions, record versions, commands) and, only where a device needs logic that data cannot
express, a small codec crate.

These live in their own repository on purpose. The Maverick app does not bundle device connectors,
and the core does not depend on them; a connector is imported, not built in. That keeps device
support something a user can add, update, or remove without touching the app, and it keeps the core
honest about the boundary, because a connector that cannot reach into the core is one the core
cannot come to depend on. The contract a connector is written against, and the argument for why it
is shaped this way, is documented in the core repo:
[docs/connectors.md](https://github.com/sennnen/maverick/blob/main/docs/connectors.md). The WHOOP
protocol facts every manifest here cites, each with a confidence tag, are in
[docs/protocol/whoop.md](https://github.com/sennnen/maverick/blob/main/docs/protocol/whoop.md).

## What is here

- `whoop4/` — WHOOP 4.0 (gen4 wire). Realtime and command subset; the historical V24 record and the
  learned skin-temperature codec land later.
- `whoop5/` — WHOOP 5.0 and MG (gen5 wire). Realtime and command subset, the feature-flag enable
  sequence, and the standard heart-rate profile.

Nothing here is confirmed against physical hardware yet. Every fact carries the confidence tag it
was given in the core's protocol ledger, and the tags flip to hardware-verified as real captures
confirm them.

## Importing a connector

A connector is data plus, optionally, a small Rust crate, so importing one is a copy or a
dependency, whichever the target prefers:

- The Maverick app reads the manifests directly. Point it at a checkout of this repository, or
  vendor the folders you want.
- To develop or test the core against a known set of manifests, check this repository out alongside
  it. The core does not bundle these; it reads them for development only, and the shipped app imports
  connectors rather than embedding them.

The step-by-step for writing one is in [docs/authoring.md](docs/authoring.md).

## Validating a manifest

Two checks. The shallow structural one runs here with only a Python interpreter, and is what this
repository's CI uses:

    python3 tools/validate.py

The deep one runs a manifest through the core's real `mav-codec` schema. Check the core out
alongside this repository and run its example tool against this directory:

    cargo run -p mav-codec --example validate_manifests -- ../maverick-connectors

The dependency runs one way: a connector validates against `mav-codec`, and `mav-codec` never learns
about any specific device, which is the whole point.

## The one connector the app may ship with

The single exception to "the app bundles no connectors" is a generic Bluetooth heart-rate connector
for the standard GATT Heart Rate profile (`0x180D` / `0x2A37`). That profile is not a device family,
it is an open standard any chest strap or watch implements the same way, so a zero-configuration
fallback for it can live in the app without turning the app into a home for device-specific code.
Anything that decodes a proprietary format belongs here instead.

---

Independent and unofficial. Not affiliated with, endorsed by, or sponsored by WHOOP, Inc. "WHOOP"
names the hardware these connectors interoperate with.
