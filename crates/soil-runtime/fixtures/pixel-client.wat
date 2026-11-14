;; A Spore that owns its pixels: it fills a host-sized RGBA frame and hands the
;; bytes over. No JSON, no scene document, no host rasterizer.
;;
;; The extent is the host's (SurfaceSize::Inner = 480x640), so the guest fills
;; exactly that and lets the host validate. Red ramps across the first row and
;; that row is copied down the frame, which keeps the fixture inside the default
;; fuel budget while still exposing a wrong stride or a transposed blit.
(module
 (import "soil:canvas/surface@0.1.0" "present_rgba"
  (func $present_rgba (param i32 i32) (result i32)))
 (memory (export "memory") 20)
 (func (export "run") (result i32)
  (local $ptr i32)
  (local $x i32)
  (local $dst i32)
  (local.set $ptr (i32.const 0))
  (loop $row
   (local.set $x
    (i32.rem_u (i32.div_u (local.get $ptr) (i32.const 4)) (i32.const 256)))
   (i32.store8 (local.get $ptr) (local.get $x))
   (i32.store8 (i32.add (local.get $ptr) (i32.const 1)) (i32.const 32))
   (i32.store8 (i32.add (local.get $ptr) (i32.const 2)) (i32.const 200))
   (i32.store8 (i32.add (local.get $ptr) (i32.const 3)) (i32.const 255))
   (local.set $ptr (i32.add (local.get $ptr) (i32.const 4)))
   (br_if $row (i32.lt_u (local.get $ptr) (i32.const 1920))))
  (local.set $dst (i32.const 1920))
  (loop $copy
   (memory.copy (local.get $dst) (i32.const 0) (i32.const 1920))
   (local.set $dst (i32.add (local.get $dst) (i32.const 1920)))
   (br_if $copy (i32.lt_u (local.get $dst) (i32.const 1228800))))
  (call $present_rgba (i32.const 0) (i32.const 1228800))))
