# Reactor/Home parent-only bounded benchmark and rollback

**HOLD: do not deploy until parent explicitly clears presenter memory issue
reversal-xf1d.** Fourteen minutes flat RSS is not the requested85minute clearance.
No hardware actions were performed by Soil. Parent reports physical D-pad works
on navigation b3f9; Home172 and animation remain device-unqualified.

## Frozen files (local, under this worktree)

- `target/reactor-frozen/soil-canvas`: static stripped ARM64,
  `1fb7666c7681d63a1633cb1e1e8bb7e60a8fcb56f9199bf8f180a6a86351c4d5`
- `target/reactor-frozen/animated-battery-canvas.wasm`:
  `eaa09d7a44be3cbdbca0700f2627d12255eb2d84fa4f346e931e851bc84a51be`
- `target/navigation-frozen/soil-canvas` rollback:
  `b3f9b0488bae67fbb4e9df43a5f3cc1bd3bd0835b0e3251f3c8242092e172c09`
- Existing battery guest rollback `target/battery-canvas.wasm`:
  `f2da070c80d60f0426d77c3f8d368ba325c5d9a6a4bfed6f25d705640862214f`

Parent stage these separately in RAM as `/run/soil-canvas-reactor`,
`/run/animated-battery-canvas.wasm`, retain `/run/soil-canvas-navigation` and
`/run/battery-canvas.wasm`. Verify sha256sum after transfer; chmod755 binary.
Do not overwrite navigation binary/guest or touch presenter/provider/lid services.
Stage `scripts/reactor-benchmark.sh` as `/run/reactor-benchmark.sh`.

## Before release of keypad reader

Record exact existing producer PIDs, `/proc/PID/cmdline`, logs, output paths,
user/environment and launch mechanism from parent deployment receipt
`/tmp/cat-navigation-deploy.log`. Parent confirms presenters are healthy and
clears test. Capture presenter RSS as well as producer RSS. Existing producer
is NOT the presenter OOM source on current evidence.

Only stop the identified **inner producer**, not provider or presenter, before
new inner connects. Keypad reads are destructive: no competing inner reader,
including detached benchmarks. Outer has no keypad grant. Detached outer can
coexist, though for clean CPU measurements stop identified old outer producer
as well. Never `pkill` by broad process name.

## 20second bounded test (30second watchdog)

On phone, after parent clearance and releasing inner reader:

```sh
KEYPAD_READER_RELEASED=yes sh /run/reactor-benchmark.sh
```

This defaults to detached `/tmp/reactor-bench-inner.bgr` and
`/tmp/reactor-bench-outer.bgr`, leaving presenter input paths untouched. For the
same bounded **physical visible** test, parent chooses exact already-recorded
presenter input paths (no path guess):

```sh
: "${INNER_OUTPUT:?set exact current inner presenter BGR path}"
: "${OUTER_OUTPUT:?set exact current outer presenter BGR path}"
KEYPAD_READER_RELEASED=yes INNER_OUT="$INNER_OUTPUT" OUTER_OUT="$OUTER_OUTPUT" \
  sh /run/reactor-benchmark.sh
```

Script launches BOTH surfaces with actual animated battery guest, explicit
battery grants,80ms requested sleeps; only inner gets explicit keypad grant.
It samples uptime, producer RSS/HWM/threads, raw CPU ticks, output size/hash once
per second, and terminates only its own producer PIDs after20samples (30second
watchdog). Logs `/tmp/reactor-bench.{inner.log,outer.log,samples}`. Parent also
samples its actual presenter PIDs through `/proc/PID/status` before/after.
Both live output sizes must be inner921600/outer49152 bytes. Guest time path:
host monotonic elapsed -> Wasm export tick(i64) -> separately enabled Canvas
animation import -> single-use exact host-time relay -> owner supplied-time
update -> Wasm run -> owner present. No host-render bypass.

### Physical acceptance during visible test

1. Home reactor/status present at480x640 and128x128; observable motion on Home.
2. Inner Right reaches Battery; Right reaches Device; bounded no wrap.
3. Press physical HOMEPAGE172 from secondary view: log `normalized=Home`,
   actual guest input returns main reactor/status; repeats do not synthesize
   extra Home actions. Test Back separately. Device volume popup remains hidden.
4. Outer stays passive on Home even while inner navigates. No competing reader.
5. Home timestamps change pixels; battery/Device do not animate with time.
6. Inspect logs for errors, process death/OOM, stale input. No guessed Recents,
   volume/PTT binding, measured estimate, audio/full-duplex claim.

Requested80ms is **not measured12.5fps**: loop sleeps after synchronous battery
read/render/publish, and input-triggered presents add work. Hash samples prove
only changes, not frame counts. CPU percentage requires kernel CLK_TCK and
elapsed-time normalization; record raw ticks if that constant is not verified.
Twenty seconds cannot qualify long-term presenter memory stability. Closed-lid
clock pause is NOT wired; this build must not be described as lid-aware.

## Rollback immediately after bounded test

Script does not restart old producers. Parent restarts them with saved exact
commands/environment; retain previous presenter/provider/lid processes. Frozen
navigation equivalents below require the exact prior output variables:

```sh
: "${INNER_OUTPUT:?}"; : "${OUTER_OUTPUT:?}"
/run/soil-canvas-navigation --guest /run/battery-canvas.wasm \
 --output "$INNER_OUTPUT" --frames 36000 --interval-ms 100 --ui simple-jarvis \
 --battery-socket /run/fungos-battery-provider.sock --allow-battery \
 --keypad-socket /run/fungos-keypad-provider.sock --allow-keypad --surface inner \
 > /tmp/navigation-rollback-inner.log 2>&1 &
echo "rollback_inner_pid=$!"
/run/soil-canvas-navigation --guest /run/battery-canvas.wasm \
 --output "$OUTER_OUTPUT" --frames 36000 --interval-ms 500 --ui simple-jarvis \
 --battery-socket /run/fungos-battery-provider.sock --allow-battery --surface outer \
 > /tmp/navigation-rollback-outer.log 2>&1 &
echo "rollback_outer_pid=$!"
```

These are finite parent-managed jobs, not a supervisor. Minimum nominal sleep
budgets are inner1hour and outer5hours; render/transport adds time. Monitor for
exit and explicitly restart according to parent-selected lifecycle policy.

## Supervision gap / minimal design, NOT implemented

CLI accepts1..36000frames, no infinite mode. No automatic restart loop is
installed: selecting restart-on-error, retry delay/backoff, shutdown semantics,
keypad ownership handoff and service manager is policy. A future parent-owned
supervisor can own exactly one inner child and one outer child, restart a normal
frame-budget exit, reap children before reconnect, bound failures, and stop both
on shutdown. Do not claim a permanent service until parent chooses/installs it.

Offline gates:58 workspace tests PASS serialized; all-target check and strict
Clippy PASS. Two earlier parallel runs failed existing timing-sensitive lid and
keypad fixtures; neither gate removed. Actual guest tests retain both native
extents, repeated/quantized time, changed pixels, secondary-view stability,
Home normalization and return, denied import and forged/repeated time rejection.
