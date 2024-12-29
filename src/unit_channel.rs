use crossbeam::atomic::AtomicCell;
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};
use triomphe::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Id(usize);

impl Default for Id {
    fn default() -> Self {
        static CUR: AtomicUsize = AtomicUsize::new(0);
        Self(CUR.fetch_add(1, Ordering::Relaxed))
    }
}

struct Inner {
    id: Id,
    queue: AtomicUsize,
    waker: AtomicCell<Option<Waker>>,
}

pub(crate) struct Sender(Arc<Inner>);

impl Sender {
    pub(crate) fn send(&self) {
        self.0.queue.fetch_add(1, Ordering::Relaxed);
        if let Some(waker) = self.0.waker.take() {
            waker.wake()
        }
    }

    pub(crate) fn id(&self) -> Id {
        self.0.id
    }
}

pub(crate) struct Receiver(Arc<Inner>);

impl Receiver {
    pub(crate) fn peek(&self) -> bool {
        self.0.queue.load(Ordering::Relaxed) > 0
    }

    pub(crate) fn id(&self) -> Id {
        self.0.id
    }

    pub(crate) fn sender(&self) -> Sender {
        Sender(self.0.clone())
    }
}

impl Future for Receiver {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            let cur = self.0.queue.load(Ordering::Relaxed);
            if cur > 0 {
                match self.0.queue.compare_exchange(
                    cur,
                    cur - 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break Poll::Ready(()),
                    Err(_) => (),
                }
            } else {
                match unsafe { &*self.0.waker.as_ptr() } {
                    Some(waker) if waker.will_wake(cx.waker()) => (),
                    _ => {
                        self.0.waker.store(Some(cx.waker().clone()));
                    }
                }
                break Poll::Pending
            }
        }
    }
}

pub(crate) fn channel() -> (Sender, Receiver) {
    let inner = Arc::new(Inner {
        id: Id::default(),
        queue: AtomicUsize::new(0),
        waker: AtomicCell::new(None),
    });
    (Sender(inner.clone()), Receiver(inner))
}
