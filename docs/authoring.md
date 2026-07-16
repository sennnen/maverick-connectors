# Authoring a connector

A connector teaches Maverick how to talk to one family of device. This document is the practical
guide for writing one in this repository. The schema itself, and the argument for why connectors are
shaped this way, lives in the core repo at
[docs/connectors.md](https://github.com/sennnen/maverick/blob/main/docs/connectors.md); the device
protocol facts these manifests cite are in
[docs/protocol/whoop.md](https://github.com/sennnen/maverick/blob/main/docs/protocol/whoop.md).

## The shape of a connector

A connector is a folder named for the device family, holding a `manifest.json` of static facts and,
only where a device needs logic that a file cannot express, a small codec crate. Adding a device
changes nothing in the core. If it seems to need a core change, that is a gap in the schema, and it
is fixed by widening the schema in the core repo (with an ADR), not by special-casing the device.

The manifest holds everything static: the device identity and the model strings that resolve to it,
the GATT service and characteristic UUIDs, the frame parameters, the command opcodes, the packet
map, the field layouts, the unit conversions, the historical record versions, and the enable
sequence a strap needs before it will stream. What it does not hold is anything that is learned from
a specific physical device over time, or any stateful decode; those belong in a codec. The standing
example is the WHOOP 4.0 skin-temperature anchor, which is different for every strap and does not
exist until the strap has been worn, so it cannot sit in a static file.

## Confidence tags

Nothing in these manifests has been confirmed against a physical strap yet, so every fact carries a
tag in its `confidence` or `confidence_note` field, borrowed from the core's protocol ledger:

- **XVAL** — more than one reverse-engineering source agrees.
- **ONE** — a single source asserts it.
- **JUDES** / **SERIES** — from one of the two hardware writeups, named where it matters.
- **PROV** — provisional, uncalibrated, or a reasoned guess.
- **HW** — can only be confirmed with the physical device.
- **CONFLICT** — the sources disagree, and it must be settled on hardware.

A tag is a promise about how far to trust the line, not decoration. When a real device confirms a
fact, its tag becomes hardware-verified, and that is a change to this repository, tracked here.

## Adding a device

1. Copy an existing folder as a starting point, or make a new one named for the family.
2. Fill in `manifest.json` against the schema in the core's `docs/connectors.md`, tagging each fact
   with the confidence it deserves. Keep it to wire facts; a threshold that belongs to an algorithm
   is not a wire fact and does not go here.
3. If, and only if, the device needs stateful or learned logic, add a small codec crate for it.
4. Validate (below), and open a pull request.

## Validating a manifest

Two checks, shallow and deep.

The shallow one runs here with nothing but a Python interpreter and is what this repository's CI
uses:

    python3 tools/validate.py

It confirms each manifest is well-formed, has the required keys, names a wire format the core
implements, and has an internally consistent enable sequence.

The deep one is the authoritative check, and it runs the manifest through the core's real schema.
Check out the core repository alongside this one and run its example tool against this directory:

    cargo run -p mav-codec --example validate_manifests -- ../maverick-connectors

A manifest is correct when both pass and, once there is a capture to test against, when it decodes
that capture's frames to the samples you expect.
