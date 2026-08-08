# krab_cli

The `krab` command-line tool for the
[Krab](https://github.com/krab-framework/krab) full-stack Rust web framework.

## Install

```sh
cargo install krab_cli
```

## Commands

### Project and dev workflow

| Command | Does |
|---|---|
| `krab new <name> --template <t>` | Scaffold a new project from a template |
| `krab bootstrap` | One-command local stack: build + orchestrator |
| `krab docs` | Regenerate the dev-workflow guide |
| `krab doctor --diagnostics --strict` | Aggregated workspace health checks |
| `krab env-check --strict` | Validate required and conditional environment settings |

### Governance

| Command | Does |
|---|---|
| `krab contract check` | REST/GraphQL contract conformance |
| `krab contract protocol-check` | Protocol parity across services |
| `krab db lifecycle` | Migration lifecycle validation |
| `krab db rollback` | Rollback simulation |
| `krab db drift` | Schema drift detection |
| `krab db rehearsal` | Rollback rehearsal, writes evidence |
| `krab topology doctor` | Topology hygiene checks |
| `krab topology split <domain>` | Extract a domain into a split service |
| `krab security dependency-gate` | Dependency policy gate |
| `krab release check` | Release pre-flight |
| `krab release certify --out <dir>` | Generate a release evidence bundle |

Most commands accept `--diagnostics` for verbose output and `--json` for
machine-readable results. These are the same binaries CI runs.

## Documentation

- [Dev workflow](https://github.com/krab-framework/krab/blob/main/docs/guides/dev_workflow.md)
- [Production readiness](https://github.com/krab-framework/krab/blob/main/docs/operations/production_readiness.md)
- [Release policy](https://github.com/krab-framework/krab/blob/main/RELEASE_POLICY.md)

## License

MIT
