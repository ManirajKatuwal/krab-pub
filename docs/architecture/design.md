# Krab Framework: Architecture Design

## 1. High-Level Overview
Krab follows a "Server-First, Client-Opt-In" architecture. By default, pages are rendered as static HTML on the server. Client-side interactivity is added incrementally via "Islands of Interactivity".

## 2. Core Components

### A. The Server Runtime (Krab Server)
- **Foundation**: Built on top of `hyper` (HTTP) and `tokio` (Async Runtime).
- **Router**: A trie-based router that maps URLs to file-system paths.
- **SSR Engine**: Executes Rust components on the server to produce HTML strings.
- **Data Loader**: Handles `async` data fetching on the server before rendering. Data is serialized (e.g., via `serde_json` or `rkyv`) and embedded in the HTML for hydration.

### B. The Client Runtime (Krab Client)
- **Target**: `wasm32-unknown-unknown`.
- **Reactivity System**: Fine-grained reactivity using Signals (similar to SolidJS/Leptos). No Virtual DOM diffing; updates are direct DOM manipulations.
- **Hydration**: The client runtime only "wakes up" specific interactive components (Islands). Static HTML remains untouched.

### C. The Build System (Krab CLI)
- **Pure Rust Pipeline**: No Node.js or NPM dependencies.
- **Dual Compilation**:
    1.  Compiles the App for the Server (Native binary).
    2.  Compiles the "Islands" for the Client (WASM module).
- **Asset Pipeline**:
    - **CSS**: Processed by `lightningcss` (Rust-based) for minification, prefixing, and syntax lowering.
    - **Images**: Optimized via the `image` crate (WebP conversion).
    - **Assets**: Fingerprinted and served statically.

## 3. Detailed Subsystems

### File-System Routing
Directory structure determines the URL paths.
```rust
src/
  routes/
    index.rs          // -> /
    about.rs          // -> /about
    blog/
      index.rs        // -> /blog
      [slug].rs       // -> /blog/:slug (Dynamic Route)
    api/
      users.rs        // -> /api/users (API Endpoint)
```

### Islands Architecture Implementation
Components are standard Rust functions. To make a component interactive on the client, it must be marked.

```rust
// src/components/Counter.rs

#[island] // This macro marks it for WASM compilation
pub fn Counter(initial: i32) -> impl View {
    let (count, set_count) = create_signal(initial);
    view! {
        <button on:click=move |_| set_count.update(|n| *n += 1)>
            "Count: " {count}
        </button>
    }
}
```

- **Server behavior**: Calls `Counter(initial)`, renders HTML string.
- **Client behavior**: Downloads `counter.wasm` (or a chunk), attaches event listeners to the existing HTML.

### Data Loading Pattern
Each route can export a loader function that runs *only* on the server.

```rust
// src/routes/user/[id].rs

pub async fn loader(params: Params) -> Result<User, Error> {
    // DB calls here
    db::get_user(params.id).await
}

pub fn Page(user: User) -> impl View {
    view! { <h1>{user.name}</h1> }
}
```

## 4. State Management
- **Local State**: Signals (`create_signal`).
- **Global Client State**: Context API (dependency injection for deep trees).
- **Server State**: Request-scoped context (for Headers, User Session).

## 5. Security
- **CSRF Protection**: Built-in middleware.
- **Secure Headers**: Defaults to sensible HTTP security headers (CSP, HSTS).
- **Type-Safe SQL**: Encourages `sqlx` for compile-time checked queries.
