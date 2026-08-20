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

Finally, checks that every forward version reference — `#[deprecated(since =
..)]` in Rust sources and the "As of **X**" notes in
`docs/reference/api.md` — names either the current version or the declared
`[workspace.metadata.krab] next_version`. Those references have to name a
release before it exists, so nothing stops them naming one that never ships,
and a `since` pointing at the wrong release is worse than no `since` at all.

The rules above are the enforced layout standard; the version-pin check
implements the publication preconditions in `RELEASE_POLICY.md`.
"""

import pathlib
import re


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

# The root Cargo.toml is workspace metadata, not a crate: only these top-level
# tables may appear. Anything else — [package], [dependencies],
# [dev-dependencies], [build-dependencies], [lib], [[bin]] — means someone has
# started turning the workspace root into a crate, which is the same boundary
# violation as a crate directory at the repository root.
ALLOWED_ROOT_MANIFEST_TABLES = {"workspace", "profile", "patch"}

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


# `#[deprecated(since = "0.5.0")]`, in any spacing rustfmt produces.
DEPRECATED_SINCE_RE = re.compile(r"""since\s*=\s*["'](?P<version>[0-9]+\.[0-9]+\.[0-9]+)["']""")
# `As of **0.5.0**` / `As of 0.5.0` in the API reference.
DOC_AS_OF_RE = re.compile(r"As of \*{0,2}(?P<version>[0-9]+\.[0-9]+\.[0-9]+)\*{0,2}")

# Scanned for `since = "..."`. Generated and vendored trees are excluded: the
# CLI's project templates embed version strings for the *generated* project,
# which follow their own numbering.
SOURCE_ROOTS = ("crates/framework", "crates/tooling", "services")
SOURCE_EXCLUDES = ("project_template.rs",)

API_DOC = pathlib.Path("docs/reference/api.md")


def check_forward_version_references(manifest_text: str):
    """Return failures for version references naming an unplanned release.

    A reference may name any already-released version — those are history —
    or `next_version`, the release `[Unreleased]` becomes. Naming anything
    beyond that is a claim about a release nobody has planned.
    """
    try:
        import tomllib
    except ModuleNotFoundError:
        print("WARN: tomllib unavailable (Python < 3.11); skipping version-reference check")
        return []

    parsed = tomllib.loads(manifest_text)
    current = parsed.get("workspace", {}).get("package", {}).get("version")
    next_version = (
        parsed.get("workspace", {}).get("metadata", {}).get("krab", {}).get("next_version")
    )
    if not next_version:
        return [
            "[workspace.metadata.krab] has no `next_version`. Declare the "
            "version `[Unreleased]` will ship as, so deprecations and docs "
            "cannot name a release that never happens"
        ]
    if next_version == current:
        return [
            f"[workspace.metadata.krab] next_version is {next_version!r}, the "
            f"version already in [workspace.package]. After a release, move it "
            f"forward to the next planned version"
        ]

    def parts(version: str):
        return tuple(int(component) for component in version.split("."))

    # Past versions are history and must stay as written: `since = "0.3.0"` on
    # something actually deprecated in 0.3.0 is correct forever. The only
    # unreachable claim is one naming a release *beyond* the next planned one,
    # which is either a typo or a leftover from a renumbered release.
    ceiling = parts(next_version)
    failures = []

    for root in SOURCE_ROOTS:
        for path in sorted((ROOT / root).rglob("*.rs")):
            if path.name in SOURCE_EXCLUDES:
                continue
            for match in DEPRECATED_SINCE_RE.finditer(path.read_text(encoding="utf-8")):
                found = match.group("version")
                if parts(found) > ceiling:
                    rel = path.relative_to(ROOT).as_posix()
                    failures.append(
                        f"{rel}: `since = {found!r}` is beyond next_version "
                        f"({next_version}), so it names a release that is not "
                        f"planned. Fix the annotation, or move next_version "
                        f"forward in [workspace.metadata.krab]"
                    )

    doc = ROOT / API_DOC
    if doc.is_file():
        for match in DOC_AS_OF_RE.finditer(doc.read_text(encoding="utf-8")):
            found = match.group("version")
            if parts(found) > ceiling:
                failures.append(
                    f"{API_DOC.as_posix()}: 'As of {found}' is beyond "
                    f"next_version ({next_version}), so it names a release "
                    f"that is not planned"
                )

    return failures


def check_root_manifest_is_metadata_only(manifest_text: str):
    """Return failures for crate-level sections in the root Cargo.toml.

    Uses tomllib when available and falls back to a regex over section headers,
    the same split as `parse_members`. The regex only needs the *first* segment
    of each table path (`[workspace.dependencies]` → `workspace`), so unlike
    the version-pin check it does not risk false passes on old interpreters.
    """
    try:
        import tomllib

        top_level = set(tomllib.loads(manifest_text).keys())
    except ModuleNotFoundError:
        top_level = {
            match.group(1)
            for match in re.finditer(
                r"(?m)^\s*\[\[?\s*([A-Za-z0-9_-]+)", manifest_text
            )
        }

    return [
        f"Root Cargo.toml declares [{table}]. The root manifest is workspace "
        f"metadata only ([workspace], [workspace.*], [profile.*], [patch.*]); "
        f"crate sections belong in a member under one of "
        f"{', '.join(ALLOWED_ROOTS)}"
        for table in sorted(top_level - ALLOWED_ROOT_MANIFEST_TABLES)
    ]


def main() -> int:
    if not MANIFEST.is_file():
        print(f"ERROR: workspace manifest not found at {MANIFEST}")
        return 1

    failures = []
    manifest_text = MANIFEST.read_text(encoding="utf-8")
    members = parse_members(manifest_text)
    failures.extend(check_inter_crate_versions(manifest_text))
    failures.extend(check_forward_version_references(manifest_text))
    failures.extend(check_root_manifest_is_metadata_only(manifest_text))

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
        return 1

    print(f"OK: workspace layout checks passed ({len(members)} members)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
