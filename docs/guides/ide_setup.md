# Krab IDE Setup Guide

## Recommended Editor: VS Code with rust-analyzer

### Workspace Settings

Create or update `.vscode/settings.json` in your Krab project root with these recommended settings:

```json
{
  "rust-analyzer.cargo.features": "all",
  "rust-analyzer.check.command": "clippy",
  "rust-analyzer.check.allTargets": true,
  "rust-analyzer.procMacro.enable": true,
  "rust-analyzer.procMacro.attributes.enable": true,
  "rust-analyzer.diagnostics.disabled": [],
  "rust-analyzer.imports.granularity.group": "module",
  "rust-analyzer.inlayHints.parameterHints.enable": true,
  "rust-analyzer.inlayHints.typeHints.enable": true,
  "rust-analyzer.lens.enable": true,
  "rust-analyzer.lens.run.enable": true,
  "rust-analyzer.lens.debug.enable": true,
  "rust-analyzer.cargo.buildScripts.enable": true,
  "rust-analyzer.cargo.buildScripts.overrideCommand": null,
  "editor.formatOnSave": true,
  "[rust]": {
    "editor.defaultFormatter": "rust-lang.rust-analyzer"
  }
}
```

### Recommended Extensions

| Extension | Purpose |
|-----------|---------|
| `rust-lang.rust-analyzer` | Rust language support, proc-macro expansion |
| `vadimcn.vscode-lldb` | Debugging Rust binaries |
| `tamasfe.even-better-toml` | TOML syntax highlighting for `Cargo.toml` |
| `serayuzgur.crates` | Crate dependency version management |
| `fill-labs.dependi` | Dependency updates at a glance |

---

## Macro Expansion Compatibility

### `view!` Macro

The `view!` macro is a **proc macro** that expands into `krab_core::Node` tree construction code. rust-analyzer supports expanding proc macros natively.

**How to inspect expansion:**
1. Place your cursor on the `view!` invocation
2. Run command: `rust-analyzer: Expand Macro Recursively`
3. The expanded code will show the `krab_core::Node::Element(...)` calls

**Known behavior:**
- ✅ Autocomplete works inside `view!` for attribute names and expression blocks
- ✅ Type errors in expressions `{my_var}` are reported correctly
- ✅ Mismatched closing tags produce a clear error with span pointing to the wrong tag
- ⚠️ HTML-like syntax highlighting inside `view!` requires a language server that understands proc macros — rust-analyzer does this well

### `#[server]` Macro

The `#[server]` attribute macro generates both client and server implementations.

**Known behavior:**
- ✅ The original function signature is preserved for IDE navigation
- ✅ `go to definition` works on the function name
- ✅ Non-async functions produce a clear compile error with fix suggestion
- ✅ Missing return type produces a clear compile error
- ⚠️ The generated `_handler` and `_Args` struct won't appear in autocomplete until the project is built at least once

### `#[island]` Macro

The `#[island]` attribute macro wraps components for server-side rendering with client-side hydration.

**Known behavior:**
- ✅ Generic type parameters are rejected at compile time with a clear explanation
- ✅ Multiple arguments are rejected with guidance to use a props struct
- ⚠️ The `inventory::submit!` registration won't resolve in IDE until the project is built

---

## Known Limitations

### 1. Proc Macro Server Restarts
If rust-analyzer shows stale errors after editing a proc macro, restart the proc macro server:
- Command Palette → `rust-analyzer: Restart Server`

### 2. WASM Target Conditional Compilation
Code inside `#[cfg(target_arch = "wasm32")]` blocks may not show IDE support by default. To enable:
```json
{
  "rust-analyzer.cargo.target": "wasm32-unknown-unknown"
}
```
> **Note:** This will hide server-side code hints. Toggle as needed during development.

### 3. Build Script Generated Code
The `service_frontend` crate uses `build.rs` to generate route registration code via `include!()`. This code is:
- ✅ Available after the first build (`cargo build`)
- ⚠️ Not available until `OUT_DIR` exists — run `cargo check` once to generate

### 4. Workspace Member Discovery
Ensure your `Cargo.toml` workspace includes all relevant members. rust-analyzer reads the workspace root `Cargo.toml` to discover crates.

### 5. Large Workspace Performance
For the full Krab monorepo, consider these settings to improve responsiveness:
```json
{
  "rust-analyzer.checkOnSave.overrideCommand": [
    "cargo", "check", "--message-format=json",
    "--workspace", "--all-targets"
  ],
  "rust-analyzer.cargo.cfgs": {
    "krab_dev": null
  },
  "files.watcherExclude": {
    "**/target/**": true,
    "**/dist/**": true
  }
}
```

---

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `view!` shows "unresolved macro" | Run `cargo check` once, then restart rust-analyzer |
| No autocomplete in Island components | Ensure `proc-macro = true` is set in `krab_macros/Cargo.toml` (it is by default) |
| Generated route code shows errors | Run `cargo build -p service_frontend` to populate `OUT_DIR` |
| Slow analysis in large workspace | Use `files.watcherExclude` to exclude `target/` and `dist/` |
| `#[server]` handler not found | Build the project once so the generated `_handler` function is cached |
