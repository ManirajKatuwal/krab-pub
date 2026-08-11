# Security Policy

## Reporting a vulnerability

Report vulnerabilities **privately** through
[GitHub Security Advisories](../../security/advisories) for this repository
("Report a vulnerability"). Do not open a public issue for anything you
believe is exploitable.

Include what you can of: the affected crate and version, a reproduction or
proof of concept, and the impact you believe it has. You should receive an
acknowledgement within a week.

## Supported versions

| Version | Supported |
| --- | --- |
| 0.2.x | Yes — current release line |
| < 0.2 | No |

Pre-1.0, only the latest minor release line receives security fixes.

## Scope

The five published crates (`krab_core`, `krab_macros`, `krab_client`,
`krab_cli`, `krab_orchestrator`) are in scope. The reference services under
`services/` and `examples/` are demonstration code; reports against them are
still welcome when they reveal a framework-level defect.

## Dependency policy

Dependency advisories are fixed, not suppressed: [`deny.toml`](deny.toml)
carries no `ignore` list. The single documented exception
(`RUSTSEC-2023-0071`, unreachable via the compiled drivers) is explained in
[`.cargo/audit.toml`](.cargo/audit.toml) and the
[README](README.md#multi-database-support).

For the full security architecture — authentication, secret sourcing, proxy
trust, CSRF, headers — see
[`docs/reference/security.md`](docs/reference/security.md).
