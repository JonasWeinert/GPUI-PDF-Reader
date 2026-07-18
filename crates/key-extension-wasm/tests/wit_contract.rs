//! Compatibility-transport ABI fixture.

use std::path::PathBuf;

use wit_parser::{Resolve, Type, TypeDefKind, WorldItem, WorldKey};

fn transport_wit() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("wit")
        .join("extension.wit")
}

fn is_byte_list(resolve: &Resolve, ty: Type) -> bool {
    let Type::Id(type_id) = ty else {
        return false;
    };
    matches!(resolve.types[type_id].kind, TypeDefKind::List(Type::U8))
}

#[test]
fn compatibility_transport_keeps_its_existing_byte_abi() {
    let mut resolve = Resolve::default();
    let (package_id, _) = resolve
        .push_path(transport_wit())
        .expect("compatibility transport WIT must parse");
    let package = &resolve.packages[package_id];

    assert_eq!(package.name.namespace, "key");
    assert_eq!(package.name.name, "extension-runtime");
    assert_eq!(
        package.name.version.as_ref().map(ToString::to_string),
        Some("0.1.0".into())
    );

    let world = &resolve.worlds[package.worlds["extension"]];
    let exports = world
        .exports
        .iter()
        .map(|(key, item)| {
            let WorldKey::Name(name) = key else {
                panic!("transport ABI exports only named functions");
            };
            let WorldItem::Function(function) = item else {
                panic!("transport ABI exports only functions");
            };
            (name.as_str(), function)
        })
        .collect::<Vec<_>>();

    assert_eq!(
        exports.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        ["activate", "handle-event", "deactivate"]
    );
    assert!(exports[0].1.params.is_empty());
    assert!(exports[0].1.result.is_none());
    assert_eq!(exports[1].1.params.len(), 1);
    assert!(is_byte_list(&resolve, exports[1].1.params[0].ty));
    assert!(
        exports[1]
            .1
            .result
            .is_some_and(|ty| is_byte_list(&resolve, ty))
    );
    assert!(exports[2].1.params.is_empty());
    assert!(exports[2].1.result.is_none());
}
