"""Enforce the workspace layout standard and inter-crate version pinning.

Every crate lives under one of three roots — `crates/framework/` for shared
runtime, `crates/tooling/` for CLIs and generators, `services/` for runnable
services. A crate added at the repository root, or a workspace member outside
those roots, breaks the framework-code / app-code boundary that the layout
exists to make obvious.

Also checks that each `krab_*` entry in `[workspace.dependencies]` carries a
`version` matching `[workspace.package] version`. Cargo has no
`version.workspace = true` inside `[workspace.dependencies]`, so that value is
duplicated by necessity and drifts silently — a stale pin only surfaces at
`cargo publish`, which is the worst possible moment to find out.

Implements the CI guard specified in
`internal/plans/09_workspace_structure_standard.md` §6 Phase C, and the
publication preconditions in `RELEASE_POLICY.md`.
"""

import pathlib
import re
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "Cargo.toml"

# `examples/reference_apps/` holds vendored reference applications that are real
# workspace members, compiled and tested by CI. They are `publish = false` and
# are not framework surface, but they must be members — an example that is not
# built is an example that rots, which is the state `examples/` was in before
# `islands_rpc` (five directories, five READMEs, zero lines of Rust).
ALLOWED_ROOTS = (
    "crates/framework/",
    "crates/tooling/",
    "services/",
    "examples/reference_apps/",
)

# Directories at the repository root that may exist without being crates.
# Dot-directories (.git, .github, .cargo, .vscode, ...) are skipped generically.
NON_CRATE_ROOT_DIRS = {
    "benchmarks",
    "crates",
    "dist",
    "docker",
    "docs",
    "examples",
    "internal",
    "monitoring",
    "scripts",
    "services",
    "target",
}


def parse_members(manifest_text: str):
    """Return the `[workspace] members` list.

    Uses tomllib when available and falls back to a narrow regex so the check
    still runs on interpreters older than 3.11.
    """
    try:
        import tomllib

        return tomllib.loads(manifest_text).get("workspace", {}).get("members", [])
    except ModuleNotFoundError:
        block = re.search(r"members\s*=\s*\[(.*?)\]", manifest_text, re.DOTALL)
        if not block:
            return []
        return re.findall(r'"([^"]+)"', block.group(1))


def check_inter_crate_versions(manifest_text: str):
    """Return failures for `krab_*` workspace deps that are unpinned or stale.

    Requires tomllib (Python 3.11+). On older interpreters the check is skipped
    rather than approximated — a regex over nested tables would produce false
    passes, and a false pass here is worse than no check.
    """
    try:
        import tomllib
    except ModuleNotFoundError:
        print("WARN: tomllib unavailable (Python < 3.11); skipping version-pin check")
        return []

    parsed = tomllib.loads(manifest_text)
    expected = parsed.get("workspace", {}).get("package", {}).get("version")
    if not expected:
        return ["[workspace.package] has no `version` to pin against"]

    deps = parsed.get("workspace", {}).get("dependencies", {})
    krab_deps = {n: s for n, s in deps.items() if n.startswith("krab_")}

    if not krab_deps:
        return [
            "[workspace.dependencies] declares no krab_* crates. Inter-crate "
            "dependencies belong there so `version` and `path` stay in one place; "
            "see RELEASE_POLICY.md 'Crate Publication'"
        ]

    failures = []
    for name, spec in sorted(krab_deps.items()):
        if not isinstance(spec, dict) or "version" not in spec:
            failures.append(
                f"[workspace.dependencies] {name} has no `version`. A path-only "
                f"dependency makes every dependent crate unpublishable"
            )
            continue
        if spec["version"] != expected:
            failures.append(
                f"[workspace.dependencies] {name} is pinned to "
                f"{spec['version']!r} but [workspace.package] version is "
                f"{expected!r}. Bump both together"
            )
        if "path" not in spec:
            failures.append(
                f"[workspace.dependencies] {name} has no `path`. Without it, "
                f"workspace builds resolve against crates.io instead of the "
                f"working tree"
            )
    return failures


def main() -> int:
    if not MANIFEST.is_file():
        print(f"ERROR: workspace manifest not found at {MANIFEST}")
        return 1

    failures = []
    manifest_text = MANIFEST.read_text(encoding="utf-8")
    members = parse_members(manifest_text)
    failures.extend(check_inter_crate_versions(manifest_text))

    if not members:
        print("ERROR: no workspace members parsed from Cargo.toml")
        return 1

    for member in members:
        normalised = member.replace("\\", "/").strip("/")
        if not normalised.startswith(ALLOWED_ROOTS):
            failures.append(
                f"Workspace member outside the allowed roots: {member!r}. "
                f"Move it under one of {', '.join(ALLOWED_ROOTS)}"
            )
        if not (ROOT / normalised / "Cargo.toml").is_file():
            failures.append(f"Workspace member has no Cargo.toml: {member!r}")

    # A crate directory at the repository root is a violation even when it is
    # not yet a workspace member — that is how they usually arrive.
    for entry in sorted(ROOT.iterdir()):
        if not entry.is_dir() or entry.name.startswith("."):
            continue
        if entry.name in NON_CRATE_ROOT_DIRS:
            continue
        if (entry / "Cargo.toml").is_file():
            failures.append(
                f"Crate at repository root: {entry.name}/. "
                f"Move it under one of {', '.join(ALLOWED_ROOTS)}"
            )

    if failures:
        for item in failures:
            print(f"ERROR: {item}")
        print(
            "\nLayout standard: internal/plans/09_workspace_structure_standard.md",
            file=sys.stderr,
        )
        return 1

    print(f"OK: workspace layout checks passed ({len(members)} members)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
