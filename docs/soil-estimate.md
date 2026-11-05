# Guest-mediated model estimate — reversal-oead.2.1.4

Source integration only. No estimator service, static build, standalone guest
export, deployment, hardware operation or commit was performed. Existing frozen
production executable and battery guest hashes remain unchanged.

## Separate authority

`GuestScene::with_estimate_canvas(bytes, fuel, surface, grant)` explicitly links
only Canvas plus `soil:device/estimate@0.1.0`. It does not grant battery imports.
Existing constructors do not link estimate imports. The host constructs
`EstimateGrant::new(socket, trusted_receipts, max_age) -> Result<_, EstimateFailure>`.
HAL validates the explicit socket and six-field exact tuple allowlist; no guest
chooses any socket, receipts, qualification, input voltage or temperature.

A missing grant returns Denied before transport. Each invocation permits at most
one read and one update; checks occur before a second possible socket read.
HAL's bounded read timeout remains applicable to the single permitted read.
No host-poll bypass: the actual `fixtures/estimate-canvas.wat` guest requests
read, relays the token and presents the owner surface. The existing battery
CLI/guest path remains unchanged; no production estimate CLI or trusted receipt
configuration has been added.

## Core-Wasm lowering

`wit/soil-device.wit` defines semantic types, not a component canonical ABI.
The runtime uses these explicit bounded imports:

- `read(i32 ptr, i32 capacity) -> i32`: capacity exactly24, valid exported memory
  required before transport. Returns0 after writing the record.
- Record little-endian: token i64 at0; availability u32 at8 (Fresh0, Unknown1,
  Stale2, Denied3); presence u32 at12 (bit0 percent); percent u32 at16;
  reserved zero bytes20..24. Absent numeric zero padding is not an observation.
- `update-status(i64 token) -> i32`: consumes the pending host snapshot token;
  no guest numeric/status memory is read. Invalid, replayed or old invocation
  tokens trap. Tokens are distinct by capability namespace, single-use, and
  invalidated at invocation reset. Guessing a current token cannot select data:
  only the one host-owned snapshot from that invocation can be consumed.

The host retains `Result<RemoteEstimate, EstimateFailure>`, not a guest-authored
`ModelEstimate`. Every numeric eligibility decision uses HAL `snapshot(max_age)`
at read, token consumption, and presentation. Reset discards pending/committed
snapshots. Unknown(reason) has no numeric model; Stale maps Stale, Denied maps
Denied, all other failures map Unknown. No last-good value or BatteryView fallback.

## UI semantics

Soil uses canonical reexported `ModelEstimateView` and
`CanvasSurface::update_model_estimate`. GFX/Canvas ownership remains with their
separate writer. The actual owner renderer projects canonical model-estimate
marks: `Estimated {p}%` plus `Model estimate`. Fresh/None is
`Estimated SOC unavailable`; Unknown/Stale/Denied suppress all numeric output.
Fresh means current eligible model output, NOT measured SOC, calibrated accuracy,
or accurate capacity. Existing voltage/battery data routing is unchanged.

## Verification

Final local workspace test inventory: **53 passed**, zero failures
(20lib +6battery +5canvas +1keypad +4estimate +4lid +8viewer +5WIT).
Strict workspace Clippy and workspace check passed. Soil source formatted directly.

Actual Wasm tests cover:
- separate grant denial before contact, import isolation and unchanged old guest;
- fresh synthetic57 and genuine0; all5 unknown reasons and all8 explicit errors;
- untrusted receipts, malformed/disconnected provider and recovery;
- invalid memory/capacity/token, replay across calls/frames, altered numeric and
  availability memory ignored, and one-read budget with a granted transport;
- native480x640 and128x128 actual owner frames; exact canonical Estimated57%
  and Model estimate marks; guest-rendered RGBA equals direct canonical owner;
  nonfresh frames equal None and cached57 inputs at each availability state;
- host-retained estimate expires before later consumption/presentation, clears
  numeric output and resets to Unknown. WIT parse checked.

All synthetic numbers, digests and metadata exist only in tests. No profile
numeric data, GPL estimator math or service implementation is present. HAL
contract: device-hal `docs/estimate-transport.md`; complete contract read before
transport integration. No production trusted receipt/input producer is configured.
Live input qualification remains upstream; no live estimate or capacity claim.

Logs: `/tmp/soil-estimate-tests.log`, `/tmp/soil-estimate-final-tests.log`.
Production artifacts remain SHA256:
- soil-canvas `ebd457020d58db2d933fb2857a25ea308d2731309e9e790689cc916bbf02f826`
- battery-canvas.wasm `f2da070c80d60f0426d77c3f8d368ba325c5d9a6a4bfed6f25d705640862214f`
