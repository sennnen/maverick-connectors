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
- The core repository pulls this in as a git submodule at `connectors/`, purely so its tests and the
  hardware-free development slice can run against a known set of manifests. That submodule is a
  development convenience, not a bundling step; the shipped app still imports connectors rather than
  embedding them.

## Validating a manifest

Manifests are validated against the schema in the core's `mav-codec` crate. A connector is correct
when it parses through `Manifest::from_json` and decodes its device's frames to the expected
samples. The validation harness that runs those checks depends on `mav-codec`; it does not add a
dependency the other way, which is the whole point.

## The one connector the app may ship with

The single exception to "the app bundles no connectors" is a generic Bluetooth heart-rate connector
for the standard GATT Heart Rate profile (`0x180D` / `0x2A37`). That profile is not a device family,
it is an open standard any chest strap or watch implements the same way, so a zero-configuration
fallback for it can live in the app without turning the app into a home for device-specific code.
Anything that decodes a proprietary format belongs here instead.

---

Independent and unofficial. Not affiliated with, endorsed by, or sponsored by WHOOP, Inc. "WHOOP"
names the hardware these connectors interoperate with.
