use portable_atomic::AtomicU128;
use std::{
    mem::{self, ManuallyDrop},
    sync::atomic::Ordering,
    task::Waker,
};

// thank you crossbeam
#[allow(unused)]
const fn can_transmute<A, B>() -> bool {
    (mem::size_of::<A>() == mem::size_of::<B>()) & (mem::align_of::<A>() >= mem::align_of::<B>())
}

// in the unlikely event that the size and alignment of Waker ever
// changes, this will fail at compile time
const _OK_TO_GO: bool = can_transmute::<ManuallyDrop<Option<Waker>>, u128>();

type W = ManuallyDrop<Option<Waker>>;

#[derive(Debug)]
pub(crate) struct AtomicWaker(AtomicU128);

impl AtomicWaker {
    pub(crate) fn new() -> AtomicWaker {
        unsafe {
            let w = mem::transmute::<W, u128>(ManuallyDrop::new(None));
            Self(AtomicU128::new(w))
        }
    }

    /// Register a new waker in the atomic waker. Register makes no
    /// guarantee at all about which thread will win if multiple
    /// threads race to register a waker.
    pub(crate) fn register(&self, waker: &Waker) {
        unsafe {
            loop {
                let existing = self.0.load(Ordering::Acquire);
                let mut w = mem::transmute::<u128, W>(existing);
                if w.as_ref().map(|w| w.will_wake(waker)).unwrap_or(false) {
                    break;
                }
                let cloned = mem::transmute::<W, u128>(ManuallyDrop::new(Some(waker.clone())));
                match self.0.compare_exchange(
                    existing,
                    cloned,
                    Ordering::Acquire,
                    Ordering::Acquire,
                ) {
                    Err(_) => ManuallyDrop::drop(&mut mem::transmute::<u128, W>(cloned)),
                    Ok(_) => {
                        ManuallyDrop::drop(&mut w);
                        break;
                    }
                }
            }
        }
    }

    /// Take the current waker out of the atomic waker
    pub(crate) fn take(&self) -> Option<Waker> {
        unsafe {
            let empty = mem::transmute::<W, u128>(ManuallyDrop::new(None));
            let current = self.0.swap(empty, Ordering::Release);
            ManuallyDrop::into_inner(mem::transmute::<u128, W>(current))
        }
    }
}
