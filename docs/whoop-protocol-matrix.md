# Shared WHOOP protocol evidence matrix

WC-P8 freezes the connector-local, no-std reference boundary. Maverick's protocol ledger remains
authoritative for narrative evidence; no confidence tag changed during this port. This matrix says
which facts the shared crate actually carries and the exact fixture that guards each one.

| fact | source | fixture/test | confidence |
|---|---|---|---|
| CRC-8 `0x07`, CRC-16/Modbus `0xA001`, reflected zlib CRC-32 | cross-validated surveyed decoders and standard check vector | `standard_crc_checks_match`; every frame vector | `[XVAL]` |
| gen4 4-byte envelope, length includes CRC-32 | Maverick ledger + independently generated frame | `gen4_frame_v1` bytes in `maverick_generation_goldens_decode_exactly` | `[PROV]` envelope fixture, `[XVAL]` layout |
| gen5 8-byte routed envelope and four-byte payload padding | real static hello carried by independent clients | `gen5_hello_v1` bytes in `maverick_generation_goldens_decode_exactly` | `[XVAL]` |
| inner header is type, sequence/version, command/kind | surveyed wire decoders and captures | command-response, metadata, and all record tests | `[XVAL]` |
| command responses 1=ok and 2=pending | Maverick independently generated control goldens | both `gen*_command_response_v1` bytes | `[PROV]` |
| metadata start/complete kinds 1/3 | Maverick independently generated control goldens | `maverick_metadata_boundaries_decode_exactly` | `[PROV]` |
| HISTORY_END cursor is exactly inner bytes 13..21 | real 5.0/MG offload capture from `tanarchytan/whoop-rs` | `gen5_history_end_v2` bytes in `whoop_rs_history_cursor_is_extracted_and_echoed_exactly` | `[WRS]` |
| safe offload opcodes 34/22/23; gen5 revision bytes 0/0/1 | surveyed clients, sniffed traffic, legacy manifests | command round trips and cursor-ACK tests | `[XVAL]/[SERIES]`; gen4 absent revision is manifest evidence |
| gen4 versions 5/7/9 and 12 route to reviewed legacy decoders | Maverick native decoder tests and manifest | classifier mapping | `[PROV]` |
| gen4 versions 24/25 route by inner byte 1 | real `whoop-rs` 4.0 frames | copied v24/v25 capture vectors | `[WRS]` |
| gen5 versions 18/26 route by inner byte 1 | real `whoop-rs` 5.0/MG frames | copied v18/v26 capture vectors | `[WRS]` |
| gen5 versions 20/21 route only to synthetic deep-buffer decoders | Maverick generated structural tests and manifest | classifier mapping; physiological decode intentionally absent here | `[PROV]/UNVERIFIED` |
| every other record version remains unmapped | Maverick unknown-version invariant | `whoop_rs_records_route_by_generation_and_version` | adjudicated safety rule |

The library intentionally contains no analytics, radio, filesystem, network, clock, randomness,
thread, process, SQLite, CLI, or native-platform code. `FORCE_TRIM` (opcode 25) is explicitly
rejected even by the generic command builder; the three dedicated offload helpers are
non-destructive.
