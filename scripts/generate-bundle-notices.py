#!/usr/bin/env python3
"""Generate the dependency inventory and retained notices for one app bundle."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path


ALLOWED_LICENSES = {
    "0BSD",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CC0-1.0",
    "ISC",
    "LLVM-exception",
    "MIT",
    "MIT-0",
    "NCSA",
    "Unicode-3.0",
    "Unlicense",
    "Zlib",
}
OPERATORS = {"AND", "OR", "WITH"}
PACKAGE_RE = re.compile(r"^(.+?) v([^ ]+)(?: \(.*\))?$")
NOTICE_PREFIXES = ("LICENSE", "COPYING", "COPYRIGHT", "NOTICE")


def run(root: Path, arguments: list[str]) -> str:
    result = subprocess.run(
        arguments,
        cwd=root,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        raise SystemExit(f"command failed: {' '.join(arguments)}")
    return result.stdout


def license_is_allowed(expression: str) -> bool:
    tokens = re.findall(r"[A-Za-z0-9.+-]+", expression)
    has_allowed_choice = any(token in ALLOWED_LICENSES for token in tokens)
    has_and = "AND" in tokens
    for token in tokens:
        if token in ALLOWED_LICENSES or token in OPERATORS:
            continue
        if has_allowed_choice and not has_and:
            continue
        return False
    return bool(tokens) and has_allowed_choice


def package_key(package_spec: str) -> tuple[str, str]:
    package_spec = package_spec.removesuffix(" (*)").removesuffix(" (proc-macro)")
    match = PACKAGE_RE.match(package_spec)
    if match is None:
        raise SystemExit(f"could not parse cargo package record: {package_spec}")
    return match.group(1), match.group(2)


def source_label(root: Path, package: dict[str, object]) -> str:
    manifest = Path(str(package["manifest_path"])).resolve()
    try:
        relative = manifest.parent.relative_to(root)
    except ValueError:
        source = package.get("source")
        return str(source) if source else "external path dependency"
    return f"repository:{relative.as_posix()}"


def notice_files(root: Path, package: dict[str, object]) -> list[Path]:
    manifest_dir = Path(str(package["manifest_path"])).resolve().parent
    configured = package.get("license_file")
    if configured:
        candidate = manifest_dir / str(configured)
        return [candidate] if candidate.is_file() else []
    files = [
        candidate
        for candidate in manifest_dir.iterdir()
        if candidate.is_file()
        and candidate.name.upper().startswith(NOTICE_PREFIXES)
    ]
    if not files:
        try:
            manifest_dir.relative_to(root)
        except ValueError:
            pass
        else:
            files = [root / "LICENSE"]
    return sorted(set(files), key=lambda path: path.name)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("bundle", choices=("standard", "minimal"))
    parser.add_argument("output", type=Path)
    parser.add_argument("--target")
    arguments = parser.parse_args()

    root = Path(__file__).resolve().parent.parent
    target = arguments.target or run(
        root, ["rustc", "-vV"]
    ).split("host: ", 1)[1].splitlines()[0]
    feature_arguments = [] if arguments.bundle == "standard" else ["--no-default-features"]

    tree = run(
        root,
        [
            "cargo",
            "tree",
            "--locked",
            "-p",
            "gpui-pdf-reader",
            "--target",
            target,
            *feature_arguments,
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}@@@{l}",
        ],
    )
    metadata = json.loads(
        run(
            root,
            [
                "cargo",
                "metadata",
                "--locked",
                "--format-version",
                "1",
                "--filter-platform",
                target,
                *feature_arguments,
            ],
        )
    )
    packages = {
        (package["name"], package["version"]): package
        for package in metadata["packages"]
    }

    active: dict[tuple[str, str], str] = {}
    for line in tree.splitlines():
        package_spec, separator, license_expression = line.partition("@@@")
        license_expression = license_expression.removesuffix(" (*)")
        if not separator or not license_expression:
            raise SystemExit(f"dependency has no license metadata: {line}")
        key = package_key(package_spec)
        previous = active.setdefault(key, license_expression)
        if previous != license_expression:
            raise SystemExit(f"conflicting license metadata for {key[0]} {key[1]}")
    rejected = [
        f"{name} {version}: {expression}"
        for (name, version), expression in sorted(active.items())
        if not license_is_allowed(expression)
    ]
    if rejected:
        raise SystemExit("dependencies outside the permissive policy:\n" + "\n".join(rejected))

    output = arguments.output.resolve()
    protected_outputs = {Path("/").resolve(), Path.home().resolve(), root}
    if output in protected_outputs or output in root.parents:
        raise SystemExit(
            "refusing to replace a protected notice output; choose a dedicated directory "
            "outside the repository"
        )
    target_root = (root / "target").resolve()
    if root in output.parents and target_root not in output.parents:
        raise SystemExit(
            "refusing to replace a repository directory outside target/; "
            "choose target/ or a dedicated external directory"
        )
    if output.exists():
        shutil.rmtree(output)
    licenses_output = output / "RustLicenses"
    licenses_output.mkdir(parents=True)

    inventory_rows: list[tuple[str, str, str, str, str]] = []
    copied_notice_count = 0
    for key, expression in sorted(active.items()):
        package = packages.get(key)
        if package is None:
            raise SystemExit(f"cargo metadata omitted active package {key[0]} {key[1]}")
        files = notice_files(root, package)
        retained: list[str] = []
        if files:
            package_output = licenses_output / f"{key[0]}-{key[1]}"
            package_output.mkdir()
            for index, source in enumerate(files):
                name = source.name
                destination = package_output / name
                if destination.exists():
                    destination = package_output / f"{index}-{name}"
                shutil.copy2(source, destination)
                retained.append(destination.relative_to(output).as_posix())
                copied_notice_count += 1
        inventory_rows.append(
            (
                key[0],
                key[1],
                expression,
                source_label(root, package),
                ", ".join(retained) if retained else "license expression in Cargo metadata",
            )
        )

    inventory = output / "RUST_DEPENDENCIES.tsv"
    with inventory.open("w", encoding="utf-8", newline="\n") as handle:
        handle.write("name\tversion\tlicense\tsource\tretained_notice\n")
        for row in inventory_rows:
            handle.write("\t".join(row) + "\n")

    summary = output / "README.md"
    with summary.open("w", encoding="utf-8", newline="\n") as handle:
        handle.write(f"# {arguments.bundle.capitalize()} bundle notices\n\n")
        handle.write(
            f"This inventory covers the {arguments.bundle} GPUI PDF Reader normal/build "
            f"dependency graph for `{target}`. It contains {len(inventory_rows)} unique Rust "
            f"package records and {copied_notice_count} retained license/notice files.\n\n"
        )
        handle.write(
            "`RUST_DEPENDENCIES.tsv` is generated from the locked, feature-selected Cargo "
            "graph. `RustLicenses/` retains every package-level license, copying, copyright, "
            "or notice file present in the resolved package source. Some published crates "
            "declare an SPDX license in Cargo metadata without including a separate license "
            "file; those records remain explicit in the inventory.\n\n"
        )
        handle.write(
            "The adjacent `PDFium/` directory is the complete native PDFium notice bundle. "
            "Theme provenance and licenses are under `Themes/`. The project-wide policy and "
            "human-reviewed native dependency notes are in `THIRD_PARTY_NOTICES.md`.\n"
        )

    shutil.copy2(root / "LICENSE", output / "PROJECT_LICENSE")
    shutil.copy2(root / "THIRD_PARTY_NOTICES.md", output / "THIRD_PARTY_NOTICES.md")
    shutil.copy2(root / "Cargo.lock", output / "Cargo.lock")
    shutil.copytree(root / "vendor" / "pdfium" / "licenses", output / "PDFium" / "licenses")
    shutil.copy2(root / "vendor" / "pdfium" / "LICENSE", output / "PDFium" / "LICENSE")
    shutil.copytree(root / "assets" / "themes", output / "Themes")
    print(
        f"Generated {arguments.bundle} notices for {len(inventory_rows)} packages at {output}"
    )


if __name__ == "__main__":
    main()
