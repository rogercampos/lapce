use floem::reactive::{RwSignal, Scope, SignalGet, SignalUpdate};

/// A signal listener that receives 'events' from the outside and runs the callback.  
/// This is implemented using effects and normal rw signals. This should be used when it doesn't  
/// make sense to think of it as 'storing' a value, like an `RwSignal` would typically be used for.
///  
/// Copied/Cloned listeners refer to the same listener.
#[derive(Debug)]
pub struct Listener<T: 'static> {
    cx: Scope,
    val: RwSignal<Option<T>>,
}

impl<T: Clone + 'static> Listener<T> {
    /// Creates a listener with an immediate callback. Under the hood, uses a
    /// reactive effect that fires whenever the internal signal changes from None
    /// to Some(T). This piggybacks on Floem's reactive system to get automatic
    /// batching and scheduling of the callback on the UI thread.
    pub fn new(cx: Scope, on_val: impl Fn(T) + 'static) -> Listener<T> {
        let val = cx.create_rw_signal(None);

        let listener = Listener { val, cx };
        listener.listen(on_val);

        listener
    }

    /// Construct a listener when you can't yet give it a callback.  
    /// Call `listen` to set a callback.
    pub fn new_empty(cx: Scope) -> Listener<T> {
        let val = cx.create_rw_signal(None);
        Listener { val, cx }
    }

    pub fn scope(&self) -> Scope {
        self.cx
    }

    /// Listen for values sent to this listener.      
    pub fn listen(self, on_val: impl Fn(T) + 'static) {
        self.listen_with(self.cx, on_val)
    }

    /// Listen for values sent to this listener.  
    /// Allows creating the effect with a custom scope, letting it be disposed of.  
    pub fn listen_with(self, cx: Scope, on_val: impl Fn(T) + 'static) {
        let val = self.val;

        cx.create_effect(move |_| {
            // TODO(minor): Signals could have a `take` method to avoid cloning.
            if let Some(cmd) = val.get() {
                on_val(cmd);
            }
        });
    }

    /// Send a value to the listener. Sets the internal signal which triggers
    /// the reactive effect to run the callback. Note: if called multiple times
    /// before the effect runs, only the last value is processed (signal semantics,
    /// not queue semantics). This is acceptable because commands are processed
    /// synchronously within the same reactive cycle.
    pub fn send(&self, v: T) {
        self.val.set(Some(v));
    }
}

impl<T: 'static> Copy for Listener<T> {}

impl<T: 'static> Clone for Listener<T> {
    fn clone(&self) -> Self {
        *self
    }
}
