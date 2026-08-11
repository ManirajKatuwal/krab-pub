# ADR 0003: Server Functions Are Public Endpoints

## Status

Accepted

## Context

`#[server]` functions feel like local Rust calls from the client, but once mounted they are HTTP POST endpoints under `/api/rpc/{function_name}`. Treating them as private implementation details creates unclear validation and authorization boundaries.

## Decision

Document server functions as public HTTP endpoints. Every server function must validate inputs and enforce authentication or authorization either in the function body or in the mounted router stack.

The canonical error response carries:

- `error`
- `message`
- `status_code`
- `code`

## Consequences

- Client and server error handling share one typed envelope.
- Generated examples show validation explicitly.
- Macro validation remains strict: free async functions only, `Result<T, ServerFnError>` for non-streaming functions, and only `#[server]` / `#[server(stream)]` attributes.
