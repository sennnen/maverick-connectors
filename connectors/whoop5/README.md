# WHOOP 5.0 / MG connector

Standalone public-SDK connector for WHOOP 5.0 and MG. The shared scan identity is resolved again
from the Device Information model after connection. The state machine requires OS pairing before
discovery or encrypted subscription, writes the captured gen5 hello byte-for-byte, and sends the
adjudicated R22 feature-query/flag sequence. It emits an explicit diagnostic after configuration:
those real flags do not prove that a subscription- or server-gated deep stream is available.

Native tests pin both identities, bond order, five subscriptions, hello, all ten known flag writes,
standard/custom realtime, events, responses, v18/v26 records, structurally gated v20/v21 buffers,
600-sample ABI splitting, safe history retry/cursor ACK, cancellation, reconnect, and state restore.
Nine embedded Wasm fixtures replay activation, real v18/v26, synthetic/unverified v20/v21, standard
and custom realtime, battery, and wrist state. Opcode 25 is rejected by the shared protocol helper
and never emitted.

`package-test.json` contains only a public test key and a detached signature from a temporary
external signer. Deep validation rebuilds identical Wasm, reconstructs the signed `.mavconn`,
verifies its digest, and runs every fixture. MG equivalence and deep-buffer availability remain
tagged uncertainty until this project captures its own hardware.
