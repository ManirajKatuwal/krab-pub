# API Contract Governance

This document outlines the versioning, schema, and change policies for the Krab framework API services.

## 1. Versioning Strategy

### REST API
- **URI Versioning:** All REST endpoints MUST be prefixed with `/api/v{major}`.
  - Example: `/api/v1/users`, `/api/v1/auth/login`
- **Semantic Versioning:** We follow [SemVer 2.0.0](https://semver.org/).
  - **Major (v1 -> v2):** Breaking changes (removing fields, changing types, mandatory parameters).
  - **Minor:** Backward-compatible features (new optional fields, new endpoints).
  - **Patch:** Backward-compatible bug fixes (internal only, does not affect URI).

### GraphQL API
- **Schema Evolution:** We strive for a "versionless" schema that evolves in a backward-compatible manner.
- **Breaking Changes:** Avoid whenever possible. If unavoidable, use a new field name or type.
- **Deprecation:** Use the `@deprecated` directive for fields that should no longer be used.

## 2. Schema Change Policy

### Allowed Changes (Non-Breaking)
- Adding new optional request fields.
- Adding new response fields.
- Relaxing validation constraints (e.g., increasing max length).
- Adding new API resources/endpoints.

### Breaking Changes (Requires Major Version Bump / Deprecation)
- Removing or renaming fields.
- Changing field types (e.g., string to int).
- Making an optional field mandatory.
- Adding strict validation that rejects previously valid data.

## 3. Deprecation Timeline

When a field or endpoint is deprecated:
1. **Mark as Deprecated:** Update OpenAPI/GraphQL schema with deprecation notice and alternative.
2. **Runtime Warning:** Return a `Warning` header in HTTP responses or a warning field in GraphQL extensions.
3. **Sunset Window:** Support the deprecated feature for **at least 6 months** (or 2 minor releases) before removal.
4. **Removal:** Remove the code only after the sunset window expires and usage drops to near zero.

## 4. Error Model

All API errors MUST return the standardized `ApiError` envelope:

```json
{
  "code": "ERROR_CODE",
  "message": "Human readable description",
  "details": { ... optional structured data ... },
  "request_id": "req-123456...",
  "trace_id": "..."
}
```

Common codes:
- `UNAUTHORIZED`, `FORBIDDEN`, `NOT_FOUND`
- `BAD_REQUEST`, `VALIDATION_ERROR`, `CONFLICT`
- `TOO_MANY_REQUESTS`, `INTERNAL_SERVER_ERROR`

## 5. Protocol Parity and Exposure Mode Policy

### 5.1 Parity Matrix Required
- Any operation exposed on multiple protocols MUST have a parity matrix in `plans/` documenting:
  - REST endpoint ↔ GraphQL query/mutation ↔ RPC method mapping
  - known behavioral gaps (if any)
  - source-of-record adapter

### 5.2 Behavioral Equivalence Tests
- For multi-exposed operations, authorization outcomes and response semantics MUST match by protocol.
- Parity suites are release blockers in CI.

### 5.3 Protocol Change Classification

| Change | Classification |
|---|---|
| Add new protocol adapter for existing operation | Minor |
| Remove protocol support for existing operation | **Major** + required sunset notice |
| Switch exposure mode from multi to single | **Breaking** unless no external consumers exist |

### 5.4 Deprecation by Protocol Surface
- Deprecation notices MUST explicitly name protocol surface.
- Example: REST `user.email` deprecated, migrate to GraphQL `User.emailAddress`.

### 5.5 Observability Labels
- Metrics, tracing, and logs MUST include protocol-aware attributes.
- Standard labels/fields:
  - `protocol="rest|graphql|rpc"`
  - `operation="<service.operation>"`
  - `selection_source="policy|tenant|client|default"`

### 5.6 Exposure Mode Change Policy
- Moving from `multi` to `single` mode is treated as breaking.
- Requires migration notice and sunset timeline (minimum 6 months, aligned with section 3).
