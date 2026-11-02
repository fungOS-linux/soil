#!/bin/sh
# Parent-only, after presenter clearance. Never stops existing services.
# Defaults are detached outputs; never a presenter replacement by default.
set -eu
BIN=${BIN:-/run/soil-canvas-reactor}
GUEST=${GUEST:-/run/animated-battery-canvas.wasm}
BATTERY_SOCKET=${BATTERY_SOCKET:-/run/fungos-battery-provider.sock}
KEYPAD_SOCKET=${KEYPAD_SOCKET:-/run/fungos-keypad-provider.sock}
INNER_OUT=${INNER_OUT:-/tmp/reactor-bench-inner.bgr}
OUTER_OUT=${OUTER_OUT:-/tmp/reactor-bench-outer.bgr}
LOG=${LOG:-/tmp/reactor-bench}
# Caller must stop ONLY the existing inner producer before granting this one
# destructive keypad access, even for detached-output benchmarking.
[ "${KEYPAD_READER_RELEASED:-}" = yes ] || { echo 'Release existing inner keypad reader first; set KEYPAD_READER_RELEASED=yes' >&2; exit 2; }
[ -x "$BIN" ] && [ -f "$GUEST" ] && [ -S "$BATTERY_SOCKET" ] && [ -S "$KEYPAD_SOCKET" ]
[ "$INNER_OUT" != "$OUTER_OUT" ]
i= o= timer=
cleanup() {
    [ -z "$timer" ] || kill "$timer" 2>/dev/null || :
    [ -z "$i" ] || kill "$i" 2>/dev/null || :
    [ -z "$o" ] || kill "$o" 2>/dev/null || :
    [ -z "$i" ] || wait "$i" 2>/dev/null || :
    [ -z "$o" ] || wait "$o" 2>/dev/null || :
}
trap cleanup EXIT
trap 'exit 130' INT TERM
# 36000 is the CLI maximum, NOT a supervisor. Wall-clock watchdog bounds test.
"$BIN" --guest "$GUEST" --output "$INNER_OUT" --frames 36000 --interval-ms 80 \
    --ui simple-jarvis --animate-reactor --battery-socket "$BATTERY_SOCKET" --allow-battery \
    --keypad-socket "$KEYPAD_SOCKET" --allow-keypad --surface inner > "$LOG.inner.log" 2>&1 & i=$!
"$BIN" --guest "$GUEST" --output "$OUTER_OUT" --frames 36000 --interval-ms 80 \
    --ui simple-jarvis --animate-reactor --battery-socket "$BATTERY_SOCKET" --allow-battery \
    --surface outer > "$LOG.outer.log" 2>&1 & o=$!
# Hard test limit if monitoring itself stalls. No presenter/provider is signalled.
(sleep 30; kill "$i" "$o" 2>/dev/null || :) & timer=$!
printf 'start_uptime=' > "$LOG.samples"; cut -d' ' -f1 /proc/uptime >> "$LOG.samples"
echo "inner_pid=$i outer_pid=$o" >> "$LOG.samples"
n=0
while [ "$n" -lt 20 ]; do
    sleep 1
    n=$((n + 1))
    for pid in "$i" "$o"; do
        if ! kill -0 "$pid" 2>/dev/null; then echo "early_exit pid=$pid second=$n" >> "$LOG.samples"; exit 1; fi
        echo "second=$n pid=$pid" >> "$LOG.samples"
        grep -E '^(VmRSS|VmHWM|Threads):' "/proc/$pid/status" >> "$LOG.samples" || :
        # stat fields14/15 are cumulative user/system CPU ticks; retain raw record.
        printf 'stat=' >> "$LOG.samples"; tr '\n' ' ' < "/proc/$pid/stat" >> "$LOG.samples"; echo >> "$LOG.samples"
    done
    # Atomic replacement means inode/mtime/size samples are only coarse cadence
    # evidence, NOT an exact frame count. Retain distinct pixel hashes as well.
    for path in "$INNER_OUT" "$OUTER_OUT"; do
        if [ -f "$path" ]; then
            ls -ln "$path" >> "$LOG.samples"
            sha256sum "$path" >> "$LOG.samples"
        fi
    done
done
printf 'end_uptime=' >> "$LOG.samples"; cut -d' ' -f1 /proc/uptime >> "$LOG.samples"
echo 'bounded test complete; producers intentionally terminated by cleanup' >> "$LOG.samples"
