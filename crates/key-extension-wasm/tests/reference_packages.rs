#![cfg(feature = "wasmtime-runtime")]

use std::{
    path::PathBuf,
    str::FromStr,
    thread,
    time::{Duration, Instant},
};

use key_extension_api::{
    DataValue, EventSubscription, ExtensionEffect, ExtensionEntrypoint, ExtensionEvent,
    ExtensionVersion, HostCompatibility, LifecycleState, SnapshotKind,
};
use key_extension_host::{ExtensionHost, HostError, PackageMetadata, PermissionDecision};
use key_extension_package::{
    DenyAllSignatureVerifier, LoadedPackage, PackageError, PackageLimits, PackageLoader,
    SignaturePolicy,
};
use key_extension_wasm::{
    WasmDiagnosticCode, WasmHostAdapterConfig, WasmRuntime, WasmRuntimeLimits, compile_host_adapter,
};

fn package_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("extensions")
        .join(name)
        .join("package")
}

fn package_loader(limits: PackageLimits) -> PackageLoader {
    PackageLoader::new(
        limits,
        SignaturePolicy {
            require_signed_archives: false,
            allow_unsigned_development_directories: true,
        },
    )
}

fn load(name: &str) -> LoadedPackage {
    package_loader(PackageLimits::default())
        .load_development_directory(package_root(name), &DenyAllSignatureVerifier)
        .expect("checked-in reference package must load")
}

fn wait_for_host(
    host: &mut ExtensionHost,
    mut ready: impl FnMut(&ExtensionHost) -> bool,
) -> Vec<key_extension_host::ArbitratedEffect> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut effects = Vec::new();
    loop {
        let report = host.process_tick();
        effects.extend(report.effects);
        if ready(host) || Instant::now() >= deadline {
            return effects;
        }
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn declarative_theme_pack_loads_and_uses_host_owned_contributions() {
    let package = load("reference-theme-pack");
    let manifest = package.manifest();
    assert!(matches!(
        manifest.entrypoint,
        ExtensionEntrypoint::Declarative { .. }
    ));
    assert!(package.component().is_none());
    assert!(manifest.permissions.is_empty());
    assert_eq!(manifest.contributions.commands.len(), 1);
    assert_eq!(
        manifest.contributions.menus[0].slot.as_str(),
        "view.appearance"
    );
    assert_eq!(manifest.contributions.views.len(), 1);

    let ui: serde_json::Value = serde_json::from_slice(
        package
            .ui()
            .expect("declarative package retains UI")
            .bytes(),
    )
    .expect("reference UI is valid JSON");
    assert_eq!(ui["tokens_only"], true);

    let extension = manifest.id.clone();
    let mut host = ExtensionHost::new(Default::default());
    host.install(manifest.clone(), PackageMetadata::bundled())
        .expect("declarative package installs through the common registry");
    host.activate(&extension)
        .expect("declarative package activates without executable code");
    let contributions = host.collect_contributions();
    assert_eq!(contributions.menus.len(), 1);
    assert_eq!(contributions.views.len(), 1);
    let apply = &manifest.contributions.commands[0].id;
    host.invoke_command(apply, DataValue::String("graphite".into()))
        .expect("reference command queues");
    assert_eq!(host.process_tick().processed_events, 1);
    assert!(matches!(
        host.extension_state(&extension)
            .and_then(|state| state.get("settings")),
        Some(DataValue::Record(settings))
            if settings.get("theme-preset") == Some(&DataValue::String("graphite".into()))
    ));
    host.unload(&extension)
        .expect("declarative package unloads");
    assert_eq!(host.state(&extension), Some(LifecycleState::Disabled));
    assert!(host.collect_contributions().views.is_empty());
}

#[test]
fn statistics_component_runs_through_permissioned_shared_lifecycle() {
    let package = load("reference-document-statistics");
    let manifest = package.manifest().clone();
    let ExtensionEntrypoint::WasmComponent {
        component, world, ..
    } = &manifest.entrypoint
    else {
        panic!("statistics pilot must be a Component Model package");
    };
    let component_bytes = package
        .component()
        .expect("validated package retains component bytes")
        .bytes();
    let runtime = WasmRuntime::new(WasmRuntimeLimits::default()).expect("runtime starts");
    let adapter = compile_host_adapter(
        &runtime,
        WasmHostAdapterConfig {
            extension: manifest.id.clone(),
            component: component.clone(),
            world: world.clone(),
            subscriptions: vec![
                EventSubscription::Lifecycle,
                EventSubscription::Snapshot(SnapshotKind::Document),
            ],
        },
        component_bytes,
    )
    .expect("checked-in component compiles");

    let mut host = ExtensionHost::new(Default::default());
    let capability_version = ExtensionVersion::from_str("0.1.0").expect("static version");
    for request in &manifest.capabilities.required {
        host.register_host_capability(request.id.clone(), capability_version.clone());
    }
    host.register_wasm_adapter(adapter);
    host.install(manifest.clone(), PackageMetadata::bundled())
        .expect("statistics package installs");

    assert!(matches!(
        host.activate(&manifest.id),
        Err(HostError::PermissionsRequired { ref permissions, .. })
            if permissions == &manifest.permissions
    ));
    assert!(
        host.collect_contributions().views.is_empty(),
        "unapproved packages must not contribute UI"
    );
    for request in &manifest.permissions {
        host.set_permission_decision(
            manifest.id.clone(),
            request.permission.clone(),
            PermissionDecision::Granted,
        );
    }
    let activation = host
        .activate(&manifest.id)
        .expect("explicit permission grant permits activation");
    assert_eq!(activation.capabilities.granted.len(), 2);
    assert_eq!(host.state(&manifest.id), Some(LifecycleState::Active));
    let initial = host.collect_contributions();
    assert_eq!(initial.views.len(), 1);
    assert!(initial.views[0].state.is_empty());
    wait_for_host(&mut host, |host| host.pending_event_count() == 0);

    let open = &manifest.contributions.commands[0].id;
    host.invoke_command(open, DataValue::Null)
        .expect("statistics panel command queues");
    let command_effects = wait_for_host(&mut host, |host| host.pending_event_count() == 0);
    assert_eq!(command_effects.len(), 1);
    assert_eq!(
        command_effects[0].request.effect,
        ExtensionEffect::OpenContribution {
            contribution: manifest.contributions.views[0].id.clone(),
        }
    );

    host.enqueue_host_event(
        &manifest.id,
        ExtensionEvent::SnapshotChanged {
            snapshot: SnapshotKind::Document,
            value: DataValue::Null,
        },
    )
    .expect("bounded document snapshot queues");
    let snapshot_effects = wait_for_host(&mut host, |host| {
        host.extension_state(&manifest.id)
            .and_then(|state| state.get("runtime-ready"))
            == Some(&DataValue::Boolean(true))
    });
    assert!(
        snapshot_effects.is_empty(),
        "transport pilot has no direct authority"
    );
    let published = host.collect_contributions();
    assert_eq!(published.views.len(), 1);
    assert_eq!(
        published.views[0].state.get("runtime-ready"),
        Some(&DataValue::Boolean(true)),
        "state must cross the real component boundary and survive host validation"
    );

    host.suspend(&manifest.id, "lifecycle conformance")
        .expect("guest suspends");
    assert_eq!(host.state(&manifest.id), Some(LifecycleState::Suspended));
    assert!(host.resume(&manifest.id).expect("guest resumes").is_empty());
    host.unload(&manifest.id)
        .expect("guest unloads and drops its store");
    assert_eq!(host.state(&manifest.id), Some(LifecycleState::Disabled));
}

#[test]
fn statistics_package_and_runtime_enforce_independent_component_bounds() {
    let root = package_root("reference-document-statistics");
    let component_len = std::fs::metadata(root.join("component.wasm"))
        .expect("generated component exists")
        .len();
    let package_error = package_loader(PackageLimits {
        maximum_component_bytes: component_len - 1,
        ..PackageLimits::default()
    })
    .load_development_directory(&root, &DenyAllSignatureVerifier)
    .expect_err("package boundary rejects an oversized declared component");
    assert!(matches!(
        package_error,
        PackageError::FileTooLarge { ref path, .. } if path == "component.wasm"
    ));

    let component = std::fs::read(root.join("component.wasm")).expect("component is readable");
    let source = std::fs::read_to_string(
        root.parent()
            .expect("package directory has extension root")
            .join("source/component.wat"),
    )
    .expect("auditable component source is readable");
    assert_eq!(
        wat::parse_str(source).expect("source compiles"),
        component,
        "checked-in binary must be exactly reproducible from its WAT source"
    );
    let runtime = WasmRuntime::new(WasmRuntimeLimits {
        maximum_component_bytes: component.len() - 1,
        ..WasmRuntimeLimits::default()
    })
    .expect("limits are coherent");
    assert_eq!(
        runtime
            .compile(&component)
            .expect_err("runtime bound applies")
            .code,
        WasmDiagnosticCode::ComponentTooLarge
    );

    let runtime = WasmRuntime::new(WasmRuntimeLimits {
        maximum_input_bytes: 4,
        ..WasmRuntimeLimits::default()
    })
    .expect("limits are coherent");
    let mut instance = runtime
        .instantiate(&component)
        .expect("component instantiates");
    instance.activate().expect("component activates");
    assert_eq!(
        instance
            .call_bytes("handle-event", b"12345")
            .expect_err("input is rejected before the guest boundary")
            .code,
        WasmDiagnosticCode::InputLimitExceeded
    );
    assert_eq!(instance.state(), LifecycleState::Active);

    let runtime = WasmRuntime::new(WasmRuntimeLimits {
        maximum_output_bytes: 1,
        ..WasmRuntimeLimits::default()
    })
    .expect("limits are coherent");
    let mut instance = runtime
        .instantiate(&component)
        .expect("component instantiates");
    instance.activate().expect("component activates");
    assert_eq!(
        instance
            .call_bytes("handle-event", b"{}")
            .expect_err("output is bounded before JSON deserialization")
            .code,
        WasmDiagnosticCode::OutputLimitExceeded
    );
}

#[test]
fn adversarial_component_is_reproducible_and_suspended_after_repeated_loops() {
    let package = load("reference-adversarial-loop");
    let manifest = package.manifest().clone();
    let ExtensionEntrypoint::WasmComponent {
        component, world, ..
    } = &manifest.entrypoint
    else {
        panic!("adversarial fixture must be a Component Model package");
    };
    let component_bytes = package
        .component()
        .expect("validated package retains component bytes")
        .bytes();
    let source = std::fs::read_to_string(
        package_root("reference-adversarial-loop").join("../source/component.wat"),
    )
    .expect("auditable adversarial source is readable");
    assert_eq!(
        wat::parse_str(source).expect("adversarial source compiles"),
        component_bytes,
        "checked-in hostile binary must be reproducible from its WAT source"
    );

    let runtime = WasmRuntime::new(WasmRuntimeLimits {
        fuel_per_invocation: 10_000,
        ..WasmRuntimeLimits::default()
    })
    .expect("bounded runtime starts");
    let adapter = compile_host_adapter(
        &runtime,
        WasmHostAdapterConfig {
            extension: manifest.id.clone(),
            component: component.clone(),
            world: world.clone(),
            subscriptions: vec![EventSubscription::Commands],
        },
        component_bytes,
    )
    .expect("hostile fixture compiles inside the real runtime");

    let mut host = ExtensionHost::new(Default::default());
    host.register_host_capability(
        manifest.capabilities.required[0].id.clone(),
        ExtensionVersion::from_str("0.1.0").expect("static version"),
    );
    host.register_wasm_adapter(adapter);
    host.install(manifest.clone(), PackageMetadata::bundled())
        .expect("hostile fixture installs");
    for request in &manifest.permissions {
        host.set_permission_decision(
            manifest.id.clone(),
            request.permission.clone(),
            PermissionDecision::Granted,
        );
    }
    host.activate(&manifest.id)
        .expect("activation itself stays within budget");
    let probe = &manifest.contributions.commands[0].id;
    for _ in 0..4 {
        host.invoke_command(probe, DataValue::Null)
            .expect("bounded probe event queues before suspension");
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    while host.state(&manifest.id) != Some(LifecycleState::Suspended) {
        host.process_tick();
        assert!(
            Instant::now() < deadline,
            "repeated hostile calls were not suspended"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert!(host.collect_contributions().commands.is_empty());
}

#[test]
fn native_escape_fixture_is_explicitly_native_and_contains_no_payload() {
    let package = load("reference-native-escape");
    assert!(matches!(
        package.manifest().entrypoint,
        ExtensionEntrypoint::NativeBuiltin { .. }
    ));
    assert!(package.component().is_none());
    assert!(package.ui().is_none());
}

#[test]
fn package_api_compatibility_stays_on_the_pre_stable_contract() {
    for package in [
        load("reference-theme-pack"),
        load("reference-document-statistics"),
    ] {
        let HostCompatibility { extension_api, .. } = &package.manifest().compatibility;
        assert!(
            extension_api.matches(
                &ExtensionVersion::from_str("0.1.0").expect("static extension API version")
            ),
            "{} must remain loadable by the current extension API",
            package.manifest().id
        );
    }
}
