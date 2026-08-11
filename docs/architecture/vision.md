# Krab Framework: Vision & Philosophy

## Core Mission
To provide the ultimate full-stack Rust development experience, combining the raw performance of Rust with the ease of use found in frameworks like Next.js or SvelteKit, while eliminating common friction points in current Rust web development.

## The "Krab" Difference
Existing Rust frameworks (Leptos, Dioxus, Axum) are powerful but often require significant boilerplate or suffer from slow compile-time/HMR cycles compared to the JS ecosystem. Krab aims to bridge this gap.

## Key Pillars

### 1. Developer Experience (DX) First
- **Instant HMR**: Leveraging a custom dev server and incremental compilation techniques (potentially using `cranelift` for dev builds) to achieve sub-second hot reloading.
- **Convention over Configuration**: Standard directory structure (file-system routing) that just works.
- **Unified Tooling**: A single CLI (`krab`) for new projects, dev, build, test, and deploy. No juggling `trunk`, `cargo-leptos`, `sqlx-cli` separately.

### 2. Performance by Default
- **Islands Architecture**: Ship HTML/CSS by default. Hydrate only interactive components into WASM. Drastically reduces initial bundle size compared to full SPA approaches.
- **Edge-Ready**: Designed to run on limited resources (AWS Lambda, Cloudflare Workers) or robust VPS containers without code changes.

### 3. End-to-End Type Safety
- **RPC Integration**: Server functions callable directly from client code with full type inference. No manual API glue code.
- **Database-to-UI**: Tight integration with ORMs (like SQLx or SeaORM) to propagate types directly to UI components.

## "Killer Features"
1.  **"Krab Shell"**: A visual dev-toolbar injected in development to inspect server state, DB queries, and component hierarchy (like React DevTools + Django Debug Toolbar).
2.  **Asset Co-location**: CSS, Images, and Rust code live together. Scoped CSS out of the box.
3.  **Macro-less Routing**: Reduce reliance on heavy macros for routing to speed up compilation, utilizing file-system based routing similar to Next.js.
