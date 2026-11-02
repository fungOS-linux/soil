;; Actual Canvas rendering is host owner supplied; guest requests only the
;; granted surface and forwards normalized host events, never primitive pixels.
(module
 (import "soil:canvas/surface@0.1.0" "present" (func $present (result i32)))
 (import "soil:canvas/surface@0.1.0" "navigate" (func $navigate (param i32) (result i32)))
 (func (export "run") (result i32) (call $present))
 (func (export "input") (param $key i32) (drop (call $navigate (local.get $key)))))
