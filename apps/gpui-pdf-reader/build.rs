use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .expect("reader lives below workspace root");
    let targets = [
        root.join("apps/gpui-pdf-reader/src"),
        root.join("crates/key-ui-gpui/src"),
        root.join("crates/key-editor-gpui/src"),
        root.join("crates/key-extension-gpui/src"),
        root.join("extensions/key-reference/src"),
    ];
    let mut violations = Vec::new();
    for target in targets {
        println!("cargo:rerun-if-changed={}", target.display());
        visit(&target, &mut violations);
    }
    if !violations.is_empty() {
        panic!(
            "feature UI bypasses the typed design platform:\n{}\n\
             use DesignStyled roles or a key-ui-gpui component instead",
            violations.join("\n")
        );
    }
}

fn visit(path: &Path, violations: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit(&path, violations);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            audit_file(&path, violations);
        }
    }
}

fn audit_file(path: &Path, violations: &mut Vec<String>) {
    // This single adapter is where typed renderer-independent roles are
    // intentionally translated into GPUI's low-level styling API.
    if path.file_name().is_some_and(|name| name == "style.rs") {
        return;
    }
    let Ok(source) = fs::read_to_string(path) else {
        return;
    };
    for (index, line) in source.lines().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        let bypasses_geometry = code.contains(".rounded(")
            || code.contains(".shadow(")
            || code.contains("BoxShadow {")
            || [".shadow_sm()", ".shadow_md()", ".shadow_lg()"]
                .iter()
                .any(|needle| code.contains(needle));
        let bypasses_palette = ["rgb(", "rgba(", "hsla("]
            .iter()
            .any(|needle| code.contains(needle));
        if bypasses_geometry || bypasses_palette {
            violations.push(format!("{}:{}: {}", path.display(), index + 1, code.trim()));
        }
    }
}
