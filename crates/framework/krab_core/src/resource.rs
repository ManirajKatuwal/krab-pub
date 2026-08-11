//! Client-driven async reads with observable state.
//!
//! [`Action`](crate::action::Action) covers the write side of async; `Resource`
//! covers the read side: "this component needs data that comes from a future."
//! [`create_resource`] tracks a source, runs a fetcher when it changes, and
//! exposes the result as signals.
//!
//! ```ignore
//! let user = create_resource(
//!     move || user_id.get(),
//!     |id| async move { fetch_user(id).await },
//! );
//!
//! view! {
//!     <Show when={move || user.state().get().is_ready()}>
//!         <p>{ move || user.value().get().map(|u| u.name).unwrap_or_default() }</p>
//!     </Show>
//! }
//! ```
//!
//! # Server-side rendering
//!
//! Per [ADR 0009](https://github.com/ManirajKatuwal/krab/blob/main/docs/adr/0009-resource-ssr-semantics.md),
//! a resource never polls its future on the server. Construction and every read
//! are pure signal operations: with an initial value it renders `Ready`,
//! without one it renders `Pending`, and the fetch happens in the browser after
//! hydration. Data needed for first paint belongs in the async route handler,
//! passed down as island props and into [`create_resource_with_initial`] — the
//! client then hydrates `Ready` and does **not** refetch on mount.

use crate::action::{flatten_display_errors, run_guarded, GuardedFuture};
use crate::signal::{create_effect, create_signal, untrack, ReadSignal, WriteSignal};
use std::cell::Cell;
use std::future::Future;
use std::rc::Rc;

/// Where a [`Resource`] is in its load cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceState {
    /// No completed fetch yet, or one is in flight.
    Pending,
    /// The most recent fetch succeeded; the value is in [`Resource::value`].
    Ready,
    /// The most recent fetch failed. The last good value, if any, is still in
    /// [`Resource::value`].
    Error(String),
}

impl ResourceState {
    /// The most recent fetch completed successfully.
    pub fn is_ready(&self) -> bool {
        matches!(self, ResourceState::Ready)
    }

    /// No fetch has completed yet, or a refetch is in flight.
    pub fn is_pending(&self) -> bool {
        matches!(self, ResourceState::Pending)
    }

    /// The most recent fetch failed.
    pub fn is_error(&self) -> bool {
        matches!(self, ResourceState::Error(_))
    }
}

/// An async read with `state` and `value` exposed as signals.
///
/// Created by [`create_resource`] or [`create_resource_with_initial`]. Cloning
/// is cheap and shares one fetcher and one set of signals.
///
/// The state carries no payload; the value lives in its own signal so that a
/// failed *refetch* moves `state` to `Error` while `value` keeps the last good
/// data — blanking rendered data because a refresh failed is worse than showing
/// it beside the error.
pub struct Resource<S, T: 'static> {
    state: ReadSignal<ResourceState>,
    set_state: WriteSignal<ResourceState>,
    value: ReadSignal<Option<T>>,
    set_value: WriteSignal<Option<T>>,
    source: Rc<dyn Fn() -> S>,
    fetcher: Rc<dyn Fn(S) -> GuardedFuture<T>>,
    /// Bumped on every fetch so a slow earlier request cannot overwrite a
    /// faster later one — the same discard-superseded-responses rule as
    /// [`Action`](crate::action::Action).
    generation: Rc<Cell<u64>>,
}

impl<S, T: 'static> Clone for Resource<S, T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            set_state: self.set_state.clone(),
            value: self.value.clone(),
            set_value: self.set_value.clone(),
            source: self.source.clone(),
            fetcher: self.fetcher.clone(),
            generation: self.generation.clone(),
        }
    }
}

impl<S, T> Resource<S, T>
where
    S: 'static,
    T: Clone + 'static,
{
    /// Where the resource is in its load cycle.
    pub fn state(&self) -> ReadSignal<ResourceState> {
        self.state.clone()
    }

    /// The most recent successfully fetched value, if any.
    ///
    /// Survives a failed refetch: `state` reports the [`ResourceState::Error`]
    /// while this keeps the last good data.
    pub fn value(&self) -> ReadSignal<Option<T>> {
        self.value.clone()
    }

    /// Run the fetcher again with the current source value.
    ///
    /// Reads the source through [`untrack`], so calling this inside an effect
    /// does not subscribe that effect to the source. Inert on the server, like
    /// [`Action::dispatch`](crate::action::Action::dispatch): it returns before
    /// touching any signal.
    pub fn refetch(&self) {
        let input = untrack(|| (self.source)());
        self.fetch(input);
    }

    /// Start a fetch. Superseded responses are discarded, never applied.
    ///
    /// The executor check, generation bump, and stale-discard live in
    /// [`run_guarded`], shared with [`Action::dispatch`](crate::action::Action::dispatch):
    /// with no executor nothing polls the future, so this returns before
    /// touching any signal and SSR renders the same idle shape regardless of
    /// call count.
    fn fetch(&self, input: S) {
        let set_state = self.set_state.clone();
        let set_value = self.set_value.clone();

        run_guarded(
            &self.generation,
            // Pending during a refetch as well: consumers rendering a spinner
            // off `state` see the reload, while `value` keeps the data on
            // screen.
            || self.set_state.set(ResourceState::Pending),
            || (self.fetcher)(input),
            move |outcome| match outcome {
                Ok(value) => {
                    set_value.set(Some(value));
                    set_state.set(ResourceState::Ready);
                }
                Err(message) => set_state.set(ResourceState::Error(message)),
            },
        );
    }
}

/// Create a [`Resource`] that fetches on creation and refetches when `source`
/// changes.
///
/// `source` is read inside an effect, so any signal it touches becomes a
/// dependency: when one changes, the fetcher runs again with the new source
/// value. On the server nothing fetches and the resource renders `Pending` —
/// see the module docs.
pub fn create_resource<S, T, Src, F, Fut, E>(source: Src, fetcher: F) -> Resource<S, T>
where
    S: 'static,
    T: Clone + 'static,
    Src: Fn() -> S + 'static,
    F: Fn(S) -> Fut + 'static,
    Fut: Future<Output = Result<T, E>> + 'static,
    E: std::fmt::Display + 'static,
{
    build(None, source, fetcher)
}

/// Create a [`Resource`] that starts `Ready` with a server-provided value and
/// does **not** fetch on creation.
///
/// `initial` typically arrives through island props, fetched by the async route
/// handler: the server renders `Ready`, the client hydrates `Ready` with the
/// same value, and no request is doubled on mount. Refetching-on-hydrate was
/// considered and rejected in ADR 0009 — staleness is handled by an explicit
/// [`Resource::refetch`] or a source change, not by replaying every load.
///
/// `None` behaves exactly like [`create_resource`], so a handler that may or
/// may not have the data can pass its `Option` straight through.
pub fn create_resource_with_initial<S, T, Src, F, Fut, E>(
    initial: Option<T>,
    source: Src,
    fetcher: F,
) -> Resource<S, T>
where
    S: 'static,
    T: Clone + 'static,
    Src: Fn() -> S + 'static,
    F: Fn(S) -> Fut + 'static,
    Fut: Future<Output = Result<T, E>> + 'static,
    E: std::fmt::Display + 'static,
{
    build(initial, source, fetcher)
}

fn build<S, T, Src, F, Fut, E>(initial: Option<T>, source: Src, fetcher: F) -> Resource<S, T>
where
    S: 'static,
    T: Clone + 'static,
    Src: Fn() -> S + 'static,
    F: Fn(S) -> Fut + 'static,
    Fut: Future<Output = Result<T, E>> + 'static,
    E: std::fmt::Display + 'static,
{
    let has_initial = initial.is_some();
    let (state, set_state) = create_signal(if has_initial {
        ResourceState::Ready
    } else {
        ResourceState::Pending
    });
    let (value, set_value) = create_signal(initial);

    let resource = Resource {
        state,
        set_state,
        value,
        set_value,
        source: Rc::new(source),
        fetcher: flatten_display_errors(fetcher),
        generation: Rc::new(Cell::new(0)),
    };

    // The tracked read lives here: the effect subscribes to whatever `source`
    // touches, so a source change refetches. The first run is special-cased —
    // with an initial value the data is already on screen, and fetching anyway
    // would double every load the server just did.
    let tracked = resource.clone();
    let first_run = Cell::new(true);
    create_effect(move || {
        let input = (tracked.source)();
        if first_run.replace(false) && has_initial {
            return;
        }
        tracked.fetch(input);
    });

    resource
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SSR contract from ADR 0009: construction never polls the future,
    /// and no signal is touched by a fetch that has nowhere to run — the same
    /// inertness `action::tests` pins for `dispatch`.
    #[test]
    fn a_resource_without_initial_renders_pending_on_the_server() {
        let (id, _set_id) = create_signal(1u32);
        let resource =
            create_resource(move || id.get(), |n: u32| async move { Ok::<_, String>(n) });

        assert!(resource.state().get().is_pending());
        assert_eq!(resource.value().get(), None);
    }

    #[test]
    fn a_resource_with_initial_renders_ready_on_the_server() {
        let (id, _set_id) = create_signal(1u32);
        let resource = create_resource_with_initial(
            Some(42u32),
            move || id.get(),
            |n: u32| async move { Ok::<_, String>(n) },
        );

        assert!(resource.state().get().is_ready());
        assert_eq!(resource.value().get(), Some(42));
    }

    /// A source change re-runs the tracking effect natively (effects are live
    /// under `cfg(test)`), but the fetch inside it must still be inert: state
    /// stays exactly as constructed, no matter how often the source moves.
    #[test]
    fn source_changes_and_refetch_are_inert_on_the_server() {
        let (id, set_id) = create_signal(1u32);
        let resource = create_resource_with_initial(
            Some(10u32),
            move || id.get(),
            |n: u32| async move { Ok::<_, String>(n) },
        );

        set_id.set(2);
        set_id.set(3);
        resource.refetch();

        assert!(
            resource.state().get().is_ready(),
            "no executor means no fetch: state must not move to Pending"
        );
        assert_eq!(resource.value().get(), Some(10));
    }

    #[test]
    fn clones_share_the_same_signals() {
        let (id, _set_id) = create_signal(1u32);
        let resource = create_resource_with_initial(
            Some(5u32),
            move || id.get(),
            |n: u32| async move { Ok::<_, String>(n) },
        );
        let observer = resource.clone();

        assert!(observer.state().get().is_ready());
        assert_eq!(observer.value().get(), Some(5));
    }

    #[test]
    fn a_none_initial_behaves_like_no_initial() {
        let (id, _set_id) = create_signal(1u32);
        let resource = create_resource_with_initial(
            None::<u32>,
            move || id.get(),
            |n: u32| async move { Ok::<_, String>(n) },
        );

        assert!(resource.state().get().is_pending());
        assert_eq!(resource.value().get(), None);
    }
}
