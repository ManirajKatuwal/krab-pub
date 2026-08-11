# ADR 0007: The `grpc` Feature Is Renamed, Not Implemented

## Status

**Accepted** — 2026-08-08, by the repository owner. Implemented in the same
change.

One thing was found during implementation that this ADR did not anticipate:
`ProtocolKind::parse` accepted `"grpc"` and returned `Rpc`. A service configured
with `KRAB_PROTOCOL_ENABLED=grpc` therefore started successfully, exposed Krab's
JSON-over-HTTP RPC, and reported itself as satisfying a gRPC requirement. That
alias is removed — the value is now rejected — because failing configuration
validation is the honest outcome when the transport does not exist. Covered by
`protocol_parse_rejects_grpc_instead_of_aliasing_it_to_rpc`.

## Context

`krab_core` advertises three protocols. Two are real:

| Feature | Pulls in | What it does |
|---|---|---|
| `rest` | `axum`, `tower-http`, `jsonwebtoken` | Full HTTP surface |
| `graphql` | `async-graphql` | Full GraphQL integration |
| `grpc` | **nothing** — `grpc = []` | 159 lines of enums and header parsing |

`grep -rn "tonic\|prost" --include=*.toml` over the workspace returns nothing.

What `crates/framework/krab_core/src/grpc.rs` actually contains:

- `GrpcStatusCode` — the 17 canonical status codes, with `from_u16`
- `GrpcStatus` — a code plus a message
- Timeout-header parsing (`grpc-timeout`, the `10S` / `500m` unit encoding)

There is no transport, no codegen, no `.proto` handling, no service trait, no
channel, no client. Nothing in the crate can speak gRPC to anything.

This is not worthless code. It is the vocabulary a **gateway** needs to map
between HTTP and gRPC semantics: translating status codes across a boundary,
honouring a deadline propagated in `grpc-timeout`. That is a legitimate thing to
have. It is simply not "gRPC support", which is what
[`CLAUDE.md`](../../CLAUDE.md), the protocol matrix, and the feature name all
imply.

Notably, [`CHANGELOG.md`](../../CHANGELOG.md) already describes it correctly:

> The `grpc` feature provides gRPC **status-code and metadata semantics** for
> protocol negotiation — it does not bundle a transport.

So the honest characterisation already exists in the repository. The feature
name and the reference documentation are what disagree with it. This ADR makes
the name match the description that was already written.

## Decision

**Rename the feature and module to `grpc-semantics`. Do not implement a gRPC
transport in `0.1.x`.**

1. `grpc-semantics` becomes the feature; `krab_core::grpc` becomes
   `krab_core::grpc_semantics`, with a deprecated `pub use` alias at the old
   path.
2. `grpc` remains as a deprecated feature alias (`grpc = ["grpc-semantics"]`)
   for one minor version, per the breaking-change policy. Removable no earlier
   than `0.2.0`.
3. [`docs/reference/api.md`](../reference/api.md),
   [`docs/architecture/protocol_flexibility.md`](../architecture/protocol_flexibility.md),
   [`CLAUDE.md`](../../CLAUDE.md), and the `README.md` feature list describe it
   as gateway semantics, not a transport.
4. `krab contract protocol-check` stops reporting gRPC as an available
   transport in the protocol matrix.
5. A real `tonic`-based transport, if wanted, is a separate plan with its own
   ADR. It is not a rename away — it needs `tonic`, `prost`, a build-time
   `.proto` step, a service-registration story, and a `cargo-deny` pass over a
   substantial new dependency tree.

## Consequences

**The protocol matrix stops claiming a capability the framework does not have.**
This is the point. A user selecting Krab partly for gRPC would discover the gap
only after adopting it.

**`docs/architecture/protocol_flexibility.md` and ADR 0004 need review.** ADR
0004 routes `/api/v1/rpc/*` to "RPC" — Krab's own JSON-over-HTTP server
functions, which is accurate and unaffected. But any text presenting REST /
GraphQL / gRPC as three peers needs correcting to REST / GraphQL / RPC, with
gRPC semantics as gateway support.

**Nothing downstream breaks.** No crate is published (see ADR 0005's sibling
finding and `internal/plans/framework_viability.md` C1; internal planning
document, not distributed), so the deprecation
alias is a formality here rather than a real compatibility bridge. It is kept
anyway because the policy applies uniformly and the cost is one line.

**The door to real gRPC stays open**, and reopening it is now honest work rather
than filling in a name that already promised the result.

## Alternatives considered

**Implement a `tonic` transport now.** The scope is a plan of its own, and doing
it under a remediation whose purpose is to close the gap between claims and
reality would mean adding a large dependency tree to justify a name. Rejected
for `0.1.x`; not rejected in principle.

**Delete `grpc.rs` entirely.** The status-code mapping and timeout parsing are
used by protocol negotiation and are genuinely useful at a gateway. Deleting
working code to resolve a naming problem is the wrong trade.

**Keep the name and document the limitation.** Rejected on the same grounds as
ADR 0006's equivalent option: a feature called `grpc` sets an expectation at the
point of `cargo add`, which is before anyone reads the caveat. The name is the
documentation most users see.

## References

- Module: `crates/framework/krab_core/src/grpc.rs` (159 LOC)
- Feature declaration: `crates/framework/krab_core/Cargo.toml`
- Already-correct description: [`CHANGELOG.md`](../../CHANGELOG.md) `[Unreleased]` → Added
- Protocol selection: [ADR 0004](0004-protocol-selection-by-explicit-endpoint.md)
- Remediation plan: `internal/plans/framework_viability.md` Phase 5 (internal
  planning document, not distributed)
