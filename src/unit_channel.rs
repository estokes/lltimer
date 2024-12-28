use crate::atomic_waker::AtomicWaker;
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll},
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

#[derive(Debug)]
struct Inner {
    id: Id,
    filled: AtomicBool,
    waker: AtomicWaker,
}

pub(crate) struct Sender(Arc<Inner>);

impl Sender {
    pub(crate) fn send(&self) -> bool {
        let missed = self
            .0
            .filled
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_err();
        if let Some(waker) = self.0.waker.take() {
            waker.wake()
        }
        missed
    }

    pub(crate) fn id(&self) -> Id {
        self.0.id
    }
}

pub(crate) struct Receiver(Arc<Inner>);

impl Receiver {
    pub(crate) fn is_filled(&self) -> bool {
        self.0.filled.load(Ordering::Relaxed)
    }

    pub(crate) fn id(&self) -> Id {
        self.0.id
    }

    pub(crate) fn reset(&self) {
        self.0.filled.store(false, Ordering::Relaxed);
    }

    pub(crate) fn sender(&self) -> Sender {
        Sender(self.0.clone())
    }
}

impl Future for Receiver {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.waker.register(cx.waker());
        if self.0.filled.load(Ordering::Relaxed) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

pub(crate) fn channel() -> (Sender, Receiver) {
    let inner = Arc::new(Inner {
        id: Id::default(),
        filled: AtomicBool::new(false),
        waker: AtomicWaker::new(),
    });
    (Sender(inner.clone()), Receiver(inner))
}
