;; Auditable transport-only Component Model guest for the reference document
;; statistics package. There are deliberately no imports: all PDF authority is
;; negotiated and executed by the semantic host boundary.
(component
  (core module $guest
    (memory (export "memory") 1)
    ;; ExtensionUpdate JSON with one typed, bounded DataValue and no effects:
    ;; {"state":{"runtime-ready":{"type":"boolean","value":true}},"effects":[]}
    (data (i32.const 8) "{\22state\22:{\22runtime-ready\22:{\22type\22:\22boolean\22,\22value\22:true}},\22effects\22:[]}")
    (global $heap (mut i32) (i32.const 128))
    (global $events-seen (mut i32) (i32.const 0))

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

    ;; Canonical ABI result: a pointer to the pair (data pointer, byte length).
    ;; Every successfully bounded event is observed. The guest publishes a
    ;; semantic state snapshot while intentionally requesting no side effects.
    (func (export "handle-event")
      (param $event-pointer i32)
      (param $event-length i32)
      (result i32)
      global.get $events-seen
      i32.const 1
      i32.add
      global.set $events-seen
      i32.const 0
      i32.const 8
      i32.store
      i32.const 4
      i32.const 72
      i32.store
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
