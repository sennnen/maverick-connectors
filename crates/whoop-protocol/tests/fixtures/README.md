# WHOOP protocol reference fixtures

These are byte-for-byte hardware captures imported from `tanarchytan/whoop-rs`, stripped to the
wire bytes already published as Maverick's golden fixtures. They contain biometrics and timestamps
only: no device serial, advertising name, or authentication material.

| file | upstream fixture | Maverick golden | confidence |
|---|---|---|---|
| `whoop_rs_gen5_v18.hex` | `real_frames.json:v18_real_whoop5_worn` | `r20_k18_v1.json` | `[WRS]`, real 5.0/MG |
| `whoop_rs_gen5_v26.hex` | `real_frames.json:ppg_frames[0]` | `r20_k26_v1.json` | `[WRS]`, real 5.0/MG |
| `whoop_rs_gen4_v24.hex` | `real_frames.json:v24_real_whoop4_worn` | `gen4_v24_v1.json` | `[WRS]`, real 4.0 |
| `whoop_rs_gen4_v25.hex` | `real_frames.json:v25_real_whoop4_a` | `gen4_v25_v1.json` | `[WRS]`, real 4.0 |

The comparison tests validate framing and record routing only. Physiological conversion remains in
the generation connector and later parity packets. Never edit a captured vector to satisfy a test;
replace it only from a provenance-preserving upstream or hardware capture.
