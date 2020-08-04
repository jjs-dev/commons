//! High-level opinionated multi-producer multi-consumer channel.
//! Features:
//!  - Async support
//!  - `tracing` aware

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
use tracing_futures::Instrument;
use triomphe::Arc;

/// Used to send objects into channel
pub struct Sender<T> {
    /// Reference to state
    inner: Arc<Inner<T>>,
    /// See `Inner.senders_dummy`
    // wrapped in Option for Drop::drop
    dummy: Option<Arc<SenderDummyType>>,
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.dummy
            .take()
            .expect("`dummy` field should be Some(_) for all sender lifecycle");
        // maybe, we were last sender. Let's check it.
        if self.inner.senders_closed() {
            // yes, we were last. let's wake all waiting receivers.
            // they will see that the channel is closed and will handle it.
            let mut q = self.inner.q.lock().unwrap();
            for waker in q.wakers.drain(..) {
                waker.wake()
            }
        }
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            dummy: self.dummy.clone(),
        }
    }
}

/// Used to receive objects from channel
pub struct Receiver<T> {
    /// Reference to state
    inner: Arc<Inner<T>>,
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// One queue item
struct Item<T> {
    /// Value itself
    value: T,
    /// Span of request
    span: tracing::Span,
}

/// Synchronized state of channel
struct Queue<T> {
    /// Items that were not processed yet
    values: VecDeque<Item<T>>,
    /// If queue is empty it is possible that some receivers wait for values
    /// This queue stores their wakers.
    wakers: VecDeque<Waker>,
}

/// Shared state for channel
struct Inner<T> {
    q: Mutex<Queue<T>>,
    /// Used to track current sender count.
    senders_dummy: Arc<SenderDummyType>,
}

// TODO: describe why this is correct
impl<T> Inner<T> {
    /// Checks if no senders exist
    pub(crate) fn senders_closed(&self) -> bool {
        Arc::is_unique(&self.senders_dummy)
    }
}

struct SenderDummyType;

/// Creates new channel, returning one sender and one receiver.
/// Other can be created using `Clone::clone`
pub fn channel<T: Send + 'static>() -> (Sender<T>, Receiver<T>) {
    let inner = Inner {
        q: Mutex::new(Queue {
            values: VecDeque::new(),
            wakers: VecDeque::new(),
        }),
        senders_dummy: Arc::new(SenderDummyType),
    };

    let inner = Arc::new(inner);
    let tx = Sender {
        inner: inner.clone(),
        dummy: Some(inner.senders_dummy.clone()),
    };
    let rx = Receiver { inner };
    (tx, rx)
}

impl<T> Sender<T> {
    /// Sends new value to channel
    pub fn send(&self, value: T) {
        tracing::debug!("sent to channel");
        let item = Item {
            value,
            span: tracing::Span::current(),
        };
        let mut q = self.inner.q.lock().unwrap();
        q.values.push_back(item);
        if let Some(waker) = q.wakers.pop_front() {
            waker.wake();
        }
    }
}

impl<T: Send + 'static> Receiver<T> {
    /// Polls the channel for a value.
    /// It is not exposed because it would be footgun: since some task started
    /// polling, it must not interrupt this poll. Otherwise, internal waker
    /// storage will contain "dead" wakers and some wakeups will be lost,
    /// which can lead to deadlock.
    ///
    /// Returns Some(T) on success and None if channel is empty and no
    /// senders exist
    fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<Item<T>>> {
        if self.inner.senders_closed() {
            return Poll::Ready(None);
        }
        let mut q = self.inner.q.lock().unwrap();
        if let Some(item) = q.values.pop_front() {
            return Poll::Ready(Some(item));
        }
        q.wakers.push_back(cx.waker().clone());
        Poll::Pending
    }

    /// See `poll_recv` for caveats
    async fn recv(&self) -> Option<Item<T>> {
        tokio::future::poll_fn(|cx| self.poll_recv(cx)).await
    }

    /// Receives all values from the channel and processes it with
    /// provided future constructor. This future will be spawned onto
    /// current Tokio runtime in the same `tracing` span `Sender::send` was
    /// called in. Receiving is done in background task. That task stops
    /// when all Senders are dropped, and channel is empty.
    pub fn process_all<F, C>(self, mut cons: C)
    where
        C: FnMut(T) -> F + Send + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        tokio::task::spawn(async move {
            loop {
                let maybe_item = self.recv().await;
                match maybe_item {
                    None => {
                        // channel is empty, and no senders are alive. We should stop.
                        break;
                    }
                    Some(item) => {
                        let fut = cons(item.value);
                        {
                            let _enter = item.span.enter();
                            tracing::debug!("received from channel");
                        }
                        tokio::task::spawn(fut.instrument(item.span));
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn simple() {
        let (tx1, rx1) = channel::<u8>();
        let tx2 = tx1.clone();
        let rx2 = rx1.clone();
        tx1.send(1);
        tx2.send(2);
        tx1.send(57);

        // check that order is FIFO
        assert_eq!(rx2.recv().await.unwrap().value, 1);
        assert_eq!(rx1.recv().await.unwrap().value, 2);

        {
            let mut first_call = true;
            rx1.process_all(move |message| {
                assert_eq!(message, 57);
                assert!(first_call);
                first_call = false;
                async {}
            })
        }
        std::mem::drop((tx1, tx2));
        #[allow(unreachable_code)]
        rx2.process_all(|_| {
            unreachable!();
            // type inference fails otherwise
            // should be unneeded when fallback type is never,
            // but currently it's unstable feature.
            async move {}
        })
    }
}
