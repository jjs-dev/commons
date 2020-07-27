//! This library implements simple syncronization primitive MultiWake.
//! It consists of Senders and Receivers. Receiver can wait until event happens (i.e. generation increases),
//! blocking current task. Each Sender can produce an event (i.e. increase generation), waking all waiting
//! Receivers.
//! # Performance
//! Currently very poor.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

struct Inner {
    waiters: Vec<Waker>,
    generation: usize,
    senders_count: usize,
}

pub struct Sender {
    inner: Arc<Mutex<Inner>>,
}

impl Clone for Sender {
    fn clone(&self) -> Self {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.senders_count += 1;
        }
        Sender {
            inner: self.inner.clone(),
        }
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        let mut inner = self.inner.lock().unwrap();
        inner.senders_count -= 1;
    }
}

impl Sender {
    /// Increases current generation, notifying all waiting Receivers.
    pub fn wake(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.generation += 1;
        for waker in inner.waiters.drain(..) {
            waker.wake();
        }
    }
}

#[derive(Clone)]
pub struct Receiver {
    inner: Arc<Mutex<Inner>>,
    last_observed_generation: usize,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum WaitResult {
    /// Generation was incremented by a Sender
    Ok,
    /// All Senders are dropped, so no generations can be observed in future.
    Closed,
}

impl Receiver {
    /// Tries to wait for a new generation
    pub fn poll_wait(&mut self, cx: &mut Context<'_>) -> Poll<WaitResult> {
        let mut inner = self.inner.lock().unwrap();
        if self.last_observed_generation < inner.generation {
            self.last_observed_generation = inner.generation;
            return Poll::Ready(WaitResult::Ok);
        }
        if inner.senders_count == 0 {
            return Poll::Ready(WaitResult::Closed);
        }
        inner.waiters.push(cx.waker().clone());
        Poll::Pending
    }

    /// Waits for a new generation
    pub fn wait(&mut self) -> Wait<'_> {
        Wait(self)
    }
}

/// Future resolving when new generation is observed or all senders are dropped.
pub struct Wait<'a>(&'a mut Receiver);

impl Future for Wait<'_> {
    type Output = WaitResult;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.poll_wait(cx)
    }
}

/// Produces one Sender and one Receiver, tied together.
/// Use `Clone` if needed.
pub fn multiwake() -> (Sender, Receiver) {
    let inner = Inner {
        waiters: vec![],
        generation: 0,
        senders_count: 1,
    };
    let inner = Arc::new(Mutex::new(inner));

    let tx = Sender {
        inner: inner.clone(),
    };
    let rx = Receiver {
        inner,
        last_observed_generation: 0,
    };
    (tx, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cx() -> Context<'static> {
        Context::from_waker(futures_util::task::noop_waker_ref())
    }

    #[test]
    fn simple() {
        let (tx, mut rx) = multiwake();
        // at start, rx would block
        assert_eq!(rx.poll_wait(&mut make_cx()), Poll::Pending);
        tx.wake();
        // now rx should observe new generation
        assert_eq!(rx.poll_wait(&mut make_cx()), Poll::Ready(WaitResult::Ok));
        // but next poll should block again
        assert_eq!(rx.poll_wait(&mut make_cx()), Poll::Pending);
    }

    #[test]
    fn multiple_notifications_are_observed_at_once() {
        let (tx, mut rx) = multiwake();
        tx.wake();
        tx.wake();
        tx.wake();
        // first poll should succeed
        assert_eq!(rx.poll_wait(&mut make_cx()), Poll::Ready(WaitResult::Ok));
        // but second poll should be pending
        assert_eq!(rx.poll_wait(&mut make_cx()), Poll::Pending);
    }
}
