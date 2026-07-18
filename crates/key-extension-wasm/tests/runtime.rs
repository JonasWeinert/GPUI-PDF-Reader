#![cfg(feature = "wasmtime-runtime")]

use std::time::Duration;

use key_extension_api::{
    CauseContext, CauseId, EventEnvelope, EventSource, ExtensionEvent, LifecycleEvent,
    LifecycleState,
};
use key_extension_wasm::{
    WasmDiagnosticCode, WasmExtensionInstance, WasmRuntime, WasmRuntimeLimits,
};

fn component(source: &str) -> Vec<u8> {
    wat::parse_str(source).unwrap()
}

fn lifecycle_component() -> Vec<u8> {
    component(
        r#"
        (component
            (core module $guest
                (func (export "activate"))
                (func (export "deactivate"))
                (func (export "ping"))
                (func (export "trap") unreachable)
                (func (export "spin")
                    (loop $forever br $forever))
            )
            (core instance $guest (instantiate $guest))
            (func $activate (canon lift (core func $guest "activate")))
            (func $deactivate (canon lift (core func $guest "deactivate")))
            (func $ping (canon lift (core func $guest "ping")))
            (func $trap (canon lift (core func $guest "trap")))
            (func $spin (canon lift (core func $guest "spin")))
            (export "activate" (func $activate))
            (export "deactivate" (func $deactivate))
            (export "ping" (func $ping))
            (export "trap" (func $trap))
            (export "spin" (func $spin))
        )
        "#,
    )
}

fn transport_component() -> Vec<u8> {
    component(
        r#"
        (component
            (core module $guest
                (memory (export "memory") 1)
                (data (i32.const 8) "[]")
                (global $heap (mut i32) (i32.const 16))
                (func (export "cabi_realloc")
                    (param $old-pointer i32) (param $old-size i32)
                    (param $alignment i32) (param $new-size i32)
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
                    (param $event-pointer i32) (param $event-length i32)
                    (result i32)
                    i32.const 0
                    i32.const 8
                    i32.store
                    i32.const 4
                    i32.const 2
                    i32.store
                    i32.const 0)
            )
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
            (export "deactivate" (func $deactivate))
            (export "handle-event" (func $handle-event))
        )
        "#,
    )
}

fn active_instance(limits: WasmRuntimeLimits) -> WasmExtensionInstance {
    let runtime = WasmRuntime::new(limits).unwrap();
    let mut instance = runtime.instantiate(&lifecycle_component()).unwrap();
    instance.activate().unwrap();
    instance
}

#[test]
fn component_lifecycle_releases_the_store_on_unload() {
    let runtime = WasmRuntime::new(WasmRuntimeLimits::default()).unwrap();
    let compiled = runtime.compile(&lifecycle_component()).unwrap();
    assert!(compiled.source_bytes() > 0);
    let mut instance = compiled.instantiate().unwrap();
    assert_eq!(instance.state(), LifecycleState::Validated);
    assert_eq!(
        instance.call_unit("ping").unwrap_err().code,
        WasmDiagnosticCode::InvalidLifecycle
    );

    instance.activate().unwrap();
    instance.call_unit("ping").unwrap();
    instance.suspend().unwrap();
    assert_eq!(
        instance.call_unit("ping").unwrap_err().code,
        WasmDiagnosticCode::InvalidLifecycle
    );
    instance.resume().unwrap();
    instance.call_unit("ping").unwrap();
    instance.unload().unwrap();

    assert_eq!(instance.state(), LifecycleState::Disabled);
    assert!(!instance.is_loaded());
    assert_eq!(
        instance.call_unit("ping").unwrap_err().code,
        WasmDiagnosticCode::InvalidLifecycle
    );
}

#[test]
fn traps_are_structured_and_fail_only_the_instance() {
    let mut instance = active_instance(WasmRuntimeLimits::default());
    let error = instance.call_unit("trap").unwrap_err();
    assert_eq!(error.code, WasmDiagnosticCode::Trap);
    assert_eq!(instance.state(), LifecycleState::Failed);
    instance.unload().unwrap();

    // A separate store remains usable after the first extension traps.
    let mut healthy = active_instance(WasmRuntimeLimits::default());
    healthy.call_unit("ping").unwrap();
}

#[test]
fn fuel_stops_infinite_computation() {
    let limits = WasmRuntimeLimits {
        fuel_per_invocation: 1_000,
        epoch_ticks_per_invocation: 1_000,
        epoch_tick_interval: Duration::from_secs(1),
        ..WasmRuntimeLimits::default()
    };
    let mut instance = active_instance(limits);
    let error = instance.call_unit("spin").unwrap_err();
    assert_eq!(error.code, WasmDiagnosticCode::FuelExhausted);
    assert_eq!(instance.state(), LifecycleState::Failed);
}

#[test]
fn epoch_deadline_stops_long_running_calls() {
    let limits = WasmRuntimeLimits {
        fuel_per_invocation: u64::MAX,
        epoch_ticks_per_invocation: 1,
        epoch_tick_interval: Duration::from_millis(5),
        ..WasmRuntimeLimits::default()
    };
    let mut instance = active_instance(limits);
    let error = instance.call_unit("spin").unwrap_err();
    assert_eq!(error.code, WasmDiagnosticCode::DeadlineExceeded);
    assert_eq!(instance.state(), LifecycleState::Failed);
}

#[test]
fn invalid_and_oversized_components_are_rejected_before_instantiation() {
    let runtime = WasmRuntime::new(WasmRuntimeLimits {
        maximum_component_bytes: 8,
        ..WasmRuntimeLimits::default()
    })
    .unwrap();
    assert_eq!(
        runtime.compile(&[0; 9]).unwrap_err().code,
        WasmDiagnosticCode::ComponentTooLarge
    );

    let runtime = WasmRuntime::new(WasmRuntimeLimits::default()).unwrap();
    assert_eq!(
        runtime.compile(b"not a component").unwrap_err().code,
        WasmDiagnosticCode::InvalidComponent
    );
}

#[test]
fn empty_linker_rejects_every_undeclared_import() {
    let importing = component(
        r#"
        (component
            (import "forbidden" (func $forbidden))
        )
        "#,
    );
    let runtime = WasmRuntime::new(WasmRuntimeLimits::default()).unwrap();
    let error = runtime.instantiate(&importing).unwrap_err();
    assert_eq!(error.code, WasmDiagnosticCode::MissingImport, "{error}");
}

#[test]
fn initial_linear_memory_is_subject_to_store_limits() {
    let memory_hungry = component(
        r#"
        (component
            (core module $guest
                (memory 2)
                (func (export "activate"))
            )
            (core instance $guest (instantiate $guest))
            (func $activate (canon lift (core func $guest "activate")))
            (export "activate" (func $activate))
        )
        "#,
    );
    let limits = WasmRuntimeLimits {
        maximum_memory_bytes: 64 * 1024,
        maximum_input_bytes: 1024,
        maximum_output_bytes: 1024,
        ..WasmRuntimeLimits::default()
    };
    let runtime = WasmRuntime::new(limits).unwrap();
    let error = runtime.instantiate(&memory_hungry).unwrap_err();
    assert_eq!(
        error.code,
        WasmDiagnosticCode::MemoryLimitExceeded,
        "{error}"
    );
}

#[test]
fn input_is_rejected_before_crossing_the_component_boundary() {
    let mut instance = active_instance(WasmRuntimeLimits {
        maximum_input_bytes: 4,
        ..WasmRuntimeLimits::default()
    });
    assert_eq!(
        instance
            .call_bytes("handle-event", b"12345")
            .unwrap_err()
            .code,
        WasmDiagnosticCode::InputLimitExceeded
    );
    assert_eq!(instance.state(), LifecycleState::Active);
}

#[test]
fn typed_event_transport_crosses_a_real_component_boundary() {
    let runtime = WasmRuntime::new(WasmRuntimeLimits::default()).unwrap();
    let mut instance = runtime.instantiate(&transport_component()).unwrap();
    instance.activate().unwrap();
    let update = instance
        .dispatch(&EventEnvelope {
            cause: CauseContext {
                id: CauseId::new(1, 2),
                parent: None,
                depth: 0,
            },
            source: EventSource::Host,
            sequence: 1,
            event: ExtensionEvent::Lifecycle {
                event: LifecycleEvent::ApplicationReady,
            },
        })
        .unwrap();
    assert!(update.effects.is_empty());
    assert!(update.state.is_none());
}

#[test]
fn component_output_is_bounded_before_deserialization() {
    let limits = WasmRuntimeLimits {
        maximum_output_bytes: 1,
        ..WasmRuntimeLimits::default()
    };
    let runtime = WasmRuntime::new(limits).unwrap();
    let mut instance = runtime.instantiate(&transport_component()).unwrap();
    instance.activate().unwrap();
    assert_eq!(
        instance.call_bytes("handle-event", b"{}").unwrap_err().code,
        WasmDiagnosticCode::OutputLimitExceeded
    );
}
