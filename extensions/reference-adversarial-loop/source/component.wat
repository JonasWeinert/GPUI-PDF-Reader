;; Deliberately hostile no-WASI Component Model guest. Activation is cheap so
;; the package reaches the normal runtime boundary; every event then loops
;; forever and must be stopped by the host's per-invocation fuel budget.
(component
  (core module $guest
    (memory (export "memory") 1)
    (global $heap (mut i32) (i32.const 16))

    (func (export "cabi_realloc")
      (param $old-pointer i32)
      (param $old-size i32)
      (param $alignment i32)
      (param $new-size i32)
      (result i32)
      (local $result i32)
      global.get $heap
      local.tee $result
      local.get $new-size
      i32.add
      global.set $heap
      local.get $result)

    (func (export "activate"))
    (func (export "deactivate"))
    (func (export "handle-event")
      (param $event-pointer i32)
      (param $event-length i32)
      (result i32)
      (loop $forever br $forever)
      i32.const 0))

  (core instance $guest (instantiate $guest))
  (func $activate (canon lift (core func $guest "activate")))
  (func $deactivate (canon lift (core func $guest "deactivate")))
  (func $handle-event
    (param "event-json" (list u8))
    (result (list u8))
    (canon lift (core func $guest "handle-event")
      (memory $guest "memory")
      (realloc (func $guest "cabi_realloc"))))

  (export "activate" (func $activate))
  (export "handle-event" (func $handle-event))
  (export "deactivate" (func $deactivate)))
