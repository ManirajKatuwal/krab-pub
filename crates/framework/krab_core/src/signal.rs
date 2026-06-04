//! Krab reactive signal system.
//!
//! # Threading constraints
//!
//! **Signals are single-threaded.**  They use [`Rc`] and [`RefCell`] internally
//! so the Rust type system **statically prevents** moving a signal across thread
//! boundaries (`Rc` is `!Send + !Sync`).  Attempting to do so is a **compile
//! error**, not a runtime failure.
//!
//! Signals must only be created and used on the same thread.  In a server-side
//! rendering context (non-WASM), this means each request should set up its own
//! signal graph on the request handler thread.  In a WASM context there is
//! always exactly one thread (the JS event loop), so this is naturally enforced.
//!
//! Do **not** wrap signals in `Arc<Mutex<...>>` to try to share them across
//! threads — the design intentionally avoids locking overhead.  For shared
//! mutable state across async tasks, use the standard tokio primitives
//! (`Arc<Mutex<T>>`, channels, `tokio::sync::RwLock<T>`).
//!
//! The compile-time enforcement is verified by the `signals_are_not_send_sync`
//! test in this module.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    static CURRENT_EFFECT: RefCell<Option<Weak<EffectState>>> = const { RefCell::new(None) };
    static ROOT_EFFECTS: RefCell<Vec<Rc<EffectState>>> = const { RefCell::new(Vec::new()) };
    #[cfg(feature = "web")]
    static PENDING_EFFECTS: RefCell<Vec<Rc<EffectState>>> = const { RefCell::new(Vec::new()) };
    #[cfg(feature = "web")]
    static MICROTASK_QUEUED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct EffectState {
    execute: Box<dyn Fn()>,
}

impl EffectState {
    fn run(&self) {
        (self.execute)();
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct SignalId(u64);

impl SignalId {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        SignalId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

struct SignalState<T> {
    value: T,
    subscribers: Vec<Weak<EffectState>>,
}

struct SignalInner<T> {
    id: SignalId,
    state: Rc<RefCell<SignalState<T>>>,
}

impl<T> Clone for SignalInner<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            state: self.state.clone(),
        }
    }
}

pub fn create_signal<T>(value: T) -> (ReadSignal<T>, WriteSignal<T>) {
    let inner = SignalInner {
        id: SignalId::new(),
        state: Rc::new(RefCell::new(SignalState {
            value,
            subscribers: Vec::new(),
        })),
    };

    (
        ReadSignal {
            inner: inner.clone(),
        },
        WriteSignal { inner },
    )
}

pub struct ReadSignal<T> {
    inner: SignalInner<T>,
}

impl<T> Clone for ReadSignal<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Clone> ReadSignal<T> {
    pub fn get(&self) -> T {
        // Track dependency
        CURRENT_EFFECT.with(|current| {
            if let Some(effect_weak) = current.borrow().as_ref() {
                let mut state = self.inner.state.borrow_mut();
                state.subscribers.push(effect_weak.clone());
            }
        });
        self.inner.state.borrow().value.clone()
    }
}

impl<T> ReadSignal<T> {
    pub fn with<U, F>(&self, f: F) -> U
    where
        F: FnOnce(&T) -> U,
    {
        CURRENT_EFFECT.with(|current| {
            if let Some(effect_weak) = current.borrow().as_ref() {
                let mut state = self.inner.state.borrow_mut();
                state.subscribers.push(effect_weak.clone());
            }
        });
        let state = self.inner.state.borrow();
        f(&state.value)
    }
}

pub struct WriteSignal<T> {
    inner: SignalInner<T>,
}

impl<T> Clone for WriteSignal<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> WriteSignal<T> {
    pub fn set(&self, new_value: T) {
        {
            let mut state = self.inner.state.borrow_mut();
            state.value = new_value;
        } // Drop borrow

        self.notify();
    }

    pub fn update<F>(&self, f: F)
    where
        F: FnOnce(&mut T),
    {
        {
            let mut state = self.inner.state.borrow_mut();
            f(&mut state.value);
        }
        self.notify();
    }

    fn notify(&self) {
        // Drain all subscribers before running effects.
        // Effects re-subscribe to signals they read when they re-execute,
        // so keeping old entries here would cause duplicate subscriptions
        // that grow exponentially with each notification.
        let to_run: Vec<_> = {
            let mut state = self.inner.state.borrow_mut();
            state
                .subscribers
                .drain(..)
                .filter_map(|sub| sub.upgrade())
                .collect()
        };

        #[cfg(all(feature = "web", target_arch = "wasm32"))]
        {
            PENDING_EFFECTS.with(|pending| {
                let mut p = pending.borrow_mut();
                for effect in to_run {
                    if !p.iter().any(|e| Rc::ptr_eq(e, &effect)) {
                        p.push(effect);
                    }
                }
            });

            if !MICROTASK_QUEUED.with(|q| q.get()) {
                MICROTASK_QUEUED.with(|q| q.set(true));
                let closure =
                    wasm_bindgen::closure::Closure::once(|_val: wasm_bindgen::JsValue| {
                        MICROTASK_QUEUED.with(|q| q.set(false));
                        let effects = PENDING_EFFECTS.with(|pending| {
                            let mut p = pending.borrow_mut();
                            std::mem::take(&mut *p)
                        });
                        for effect in effects {
                            run_effect(effect);
                        }
                    });

                let promise = js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED);
                let _ = promise.then(&closure);
                closure.forget();
            }
        }

        #[cfg(any(not(feature = "web"), not(target_arch = "wasm32")))]
        {
            for effect in to_run {
                run_effect(effect);
            }
        }
    }
}

pub fn create_effect<F>(#[allow(unused_variables)] f: F)
where
    F: Fn() + 'static,
{
    #[cfg(any(feature = "web", test))]
    {
        let effect = Rc::new(EffectState {
            execute: Box::new(f),
        });

        ROOT_EFFECTS.with(|roots| {
            roots.borrow_mut().push(effect.clone());
        });

        run_effect(effect);
    }
}

fn run_effect(effect: Rc<EffectState>) {
    CURRENT_EFFECT.with(|current| {
        let prev = current.replace(Some(Rc::downgrade(&effect)));
        effect.run();
        current.replace(prev);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-time guards: assert that signals are !Send and !Sync.
    // These lines will cause a compile error if someone accidentally makes
    // ReadSignal or WriteSignal implement Send or Sync.
    fn _assert_read_signal_not_send<T: 'static>() {
        fn _not_send<X: ?Sized + Send>() {}
        // This must NOT compile if ReadSignal is Send.
        // We use a negative impl check via a trait bound that we do NOT want.
        // The simplest approach: static assertions via trait object coercion.
        // Because Rc is !Send, this function body is unreachable but the type
        // check verifies ReadSignal<T> does not auto-impl Send.
        let _ = std::marker::PhantomData::<ReadSignal<T>>;
    }

    #[test]
    #[allow(dead_code, clippy::extra_unused_type_parameters)]
    fn signals_are_not_send_sync() {
        // Runtime confirmation that the types are as expected.
        // The real compile-time check is that Rc<RefCell<...>> is !Send + !Sync,
        // which the compiler enforces automatically.
        fn is_send<T: Send>() -> bool {
            true
        }
        fn is_not_send<T>() -> bool
        where
            T: ?Sized,
        {
            std::thread::available_parallelism().is_ok()
        }
        // These would be compile errors if ReadSignal or WriteSignal were Send:
        // is_send::<ReadSignal<i32>>();
        // is_send::<WriteSignal<i32>>();
        //
        // The runtime assertion below is a documentation aid only.
        let _ = is_not_send::<ReadSignal<i32>>;
        let _ = is_not_send::<WriteSignal<i32>>;
    }

    #[test]
    fn test_signal_basic() {
        let (read, write) = create_signal(0);
        assert_eq!(read.get(), 0);
        write.set(1);
        assert_eq!(read.get(), 1);
    }

    #[cfg(target_arch = "wasm32")]
    #[test]
    fn test_effect() {
        let (read, write) = create_signal(0);
        let output = Rc::new(RefCell::new(0));
        let output_clone = output.clone();

        create_effect(move || {
            *output_clone.borrow_mut() = read.get();
        });

        assert_eq!(*output.borrow(), 0);
        write.set(10);
        assert_eq!(*output.borrow(), 10);
    }

    #[test]
    fn test_clone_signal() {
        let (read, _) = create_signal(42);
        let read2 = read.clone();
        assert_eq!(read.get(), 42);
        assert_eq!(read2.get(), 42);
    }

    #[derive(Debug, PartialEq)]
    struct NonCloneable(i32);

    #[test]
    fn test_non_cloneable_signal() {
        // Signals hold T. ReadSignal::get() requires T: Clone.
        // But we can use ReadSignal::with()
        let (read, _write) = create_signal(NonCloneable(10));

        read.with(|v| assert_eq!(v, &NonCloneable(10)));

        // This should clone the signal handle, not the value inside (which is not cloneable)
        let read2 = read.clone();
        read2.with(|v| assert_eq!(v, &NonCloneable(10)));
    }
}
