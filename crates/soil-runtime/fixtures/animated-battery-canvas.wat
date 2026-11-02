;; Snapshot numbers are available for observation, but only the opaque host
;; token can update owner widgets. No guest-authored numeric data is trusted.
(module
 (import "soil:canvas/animation@0.1.0" "tick" (func $tick (param i64) (result i32)))
 (import "soil:device/battery@0.1.0" "read" (func $read (param i32 i32) (result i32)))
 (import "soil:device/battery@0.1.0" "update-status" (func $update (param i64) (result i32)))
 (import "soil:canvas/surface@0.1.0" "present" (func $present (result i32)))
 (import "soil:canvas/surface@0.1.0" "navigate" (func $navigate (param i32) (result i32)))
 (memory (export "memory") 1)
 (func (export "tick") (param $ms i64) (drop (call $tick (local.get $ms))))
 (func $refresh
  (drop (call $read (i32.const 0) (i32.const 24)))
  (drop (call $update (i64.load (i32.const 0)))))
 (func (export "run") (result i32) (call $refresh) (call $present))
 (func (export "input") (param $key i32)
  (drop (call $navigate (local.get $key)))))
