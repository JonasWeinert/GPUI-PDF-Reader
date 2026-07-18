//! Versioned WIT parsing and Rust/WIT capability identity tests.

use std::path::PathBuf;

use key_pdf_extension_api::{PDF_EXTENSION_API_VERSION, PdfCapability};
use wit_parser::{Resolve, TypeDefKind};

fn wit_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("wit")
        .join(format!("v{PDF_EXTENSION_API_VERSION}"))
}

#[test]
fn versioned_wit_package_parses_and_exposes_the_conformance_world() {
    let mut resolve = Resolve::default();
    let (package_id, _) = resolve
        .push_path(wit_dir())
        .expect("versioned WIT package must parse");
    let package = &resolve.packages[package_id];

    assert_eq!(package.name.namespace, "key");
    assert_eq!(package.name.name, "pdf-extension");
    assert_eq!(
        package.name.version.as_ref().map(ToString::to_string),
        Some(PDF_EXTENSION_API_VERSION.into())
    );
    assert!(package.worlds.contains_key("full-read-only-host"));
}

#[test]
fn rust_capability_names_map_one_to_one_to_wit_cases_and_interfaces() {
    let mut resolve = Resolve::default();
    let (package_id, _) = resolve
        .push_path(wit_dir())
        .expect("versioned WIT package must parse");
    let package = &resolve.packages[package_id];
    let types_id = package.interfaces["types"];
    let capability_type = resolve.interfaces[types_id].types["capability"];
    let TypeDefKind::Enum(capabilities) = &resolve.types[capability_type].kind else {
        panic!("WIT capability type must remain an enum");
    };
    let wit_cases = capabilities
        .cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(wit_cases.len(), PdfCapability::ALL.len());
    for capability in PdfCapability::ALL {
        assert_eq!(
            PdfCapability::from_name(capability.as_str()),
            Some(capability)
        );
        assert!(wit_cases.contains(&capability.wit_case()));
        assert!(package.interfaces.contains_key(capability.wit_case()));
        assert_eq!(capability.capability_id().as_str(), capability.as_str());
    }
}
