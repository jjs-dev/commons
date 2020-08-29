//! Reimplementation of Tokio's CancellationToken until tokio-util is released.
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
struct WakerToken(usize);

/// Comparable waker
#[derive(Debug, Clone)]
struct OrdWaker {
    waker: Waker,
    token: WakerToken,
}

/// Cancellation token's state.
#[derive(Debug)]
struct State {
    /// Tokens created using `child_token` function.
    /// When this token is cancelled, those must be cancelled too.
    children: Vec<CancellationToken>,
    /// Whether this token is cancelled.
    is_cancelled: bool,
    /// Used for assigning IDs to `OrdWaker`s
    next_generation: usize,
    /// Tasks that are waiting for this token to be cancelled
    waiters: Vec<OrdWaker>,
    /// Tasks that no longer interested in cancellation.
    /// (I.e. we should delete associated wakers from `waiters`
    /// to prevent excessive wakeups).
    stale_waiters: HashSet<WakerToken>,
}

impl State {
    fn new() -> State {
        State {
            children: Vec::new(),
            is_cancelled: false,
            next_generation: 0,
            waiters: Vec::new(),
            stale_waiters: HashSet::new(),
        }
    }

    /// Cleanups stale waiters, if needed
    fn maybe_cleanup(&mut self) {
        if self.waiters.len() >= self.stale_waiters.len() {
            return;
        }
        let mut waiters = std::mem::take(&mut self.waiters);
        waiters.retain(|ord_waker| !self.stale_waiters.contains(&ord_waker.token));
        self.stale_waiters.clear();
        self.waiters = waiters;
    }
}

#[derive(Clone, Debug)]
pub struct CancellationToken {
    /// Shared mutex to token state.
    /// This mutex is only taken for a single `poll` call.
    inner: Arc<Mutex<State>>,
}

impl CancellationToken {
    /// Creates new uncancelled token.
    pub fn new() -> CancellationToken {
        let state = State::new();
        CancellationToken {
            inner: Arc::new(Mutex::new(state)),
        }
    }

    /// Checks if this token is cancelled.
    pub fn is_cancelled(&self) -> bool {
        let state = self.inner.lock().unwrap();
        state.is_cancelled
    }

    /// Cancels token and all child tokens.
    /// If token is already cancelled, does nothing.
    pub fn cancel(&self) {
        let mut state = self.inner.lock().unwrap();
        if state.is_cancelled {
            // we do not need to do anything
            return;
        }
        // mark token as cancelled
        state.is_cancelled = true;
        // wake all tasks
        for task in state.waiters.drain(..) {
            task.waker.wake();
        }
        // cancel all children
        for child in state.children.iter() {
            child.cancel();
        }
    }

    /// Creates new token that will be cancelled when this is.
    /// If this is already cancelled, child will be immediately cancelled.
    pub fn child_token(&self) -> CancellationToken {
        let mut child_state = State::new();
        child_state.is_cancelled = self.is_cancelled();
        let token = CancellationToken {
            inner: Arc::new(Mutex::new(child_state)),
        };

        let mut my_state = self.inner.lock().unwrap();
        my_state.children.push(token.clone());
        token
    }
    /// Returns future that resolves when token is cancelled.
    /// If this token is already cancelled, future will resovle immediately.
    pub fn cancelled(&self) -> WaitForCancellationFuture {
        WaitForCancellationFuture {
            inner: &self.inner,
            last_used_waker: None,
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        CancellationToken::new()
    }
}

/// Future for the `CancellationToken::cancelled` method.
pub struct WaitForCancellationFuture<'a> {
    /// Reference to token state
    inner: &'a Mutex<State>,
    /// As an optimization, we store last waker which polled us.
    /// If we are polled with it again, there is no need in re-registering it.
    /// That way we can reduce number of accesses to `state.waiters`.
    last_used_waker: Option<OrdWaker>,
}

impl<'a> Future for WaitForCancellationFuture<'a> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = Pin::into_inner(self);
        let mut state = this.inner.lock().unwrap();
        if state.is_cancelled {
            return Poll::Ready(());
        }
        if let Some(ord_waker) = &this.last_used_waker {
            if ord_waker.waker.will_wake(cx.waker()) {
                // we are already registered for wakeup.
                return Poll::Pending;
            } else {
                // old waker is now stale.
                state.stale_waiters.insert(ord_waker.token);
                state.maybe_cleanup();
            }
        }
        let ord_token = WakerToken(state.next_generation);
        state.next_generation = state.next_generation.checked_add(1).unwrap();
        let ord_waker = OrdWaker {
            token: ord_token,
            waker: cx.waker().clone(),
        };
        this.last_used_waker = Some(ord_waker.clone());
        state.waiters.push(ord_waker);
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_simple() {
        let parent = CancellationToken::new();
        assert!(!parent.is_cancelled());

        let child = parent.child_token();
        assert!(!child.is_cancelled());
        child.cancel();
        assert!(child.is_cancelled());
        child.cancelled().await;
        assert!(!parent.is_cancelled());
        parent.cancel();
        assert!(parent.is_cancelled());
        parent.cancelled().await;
    }
}
