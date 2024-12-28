use crate::{
    unit_channel::{self, Id},
    ToTimer,
};
use crossbeam::queue::SegQueue;
use fxhash::FxHashMap;
use smallvec::{smallvec, SmallVec};
use std::{
    collections::{btree_map::Entry, BTreeMap},
    sync::LazyLock,
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::time::Instant;
use triomphe::Arc;

pub(crate) struct Ctx {
    queue: Arc<SegQueue<ToTimer>>,
    thread: JoinHandle<()>,
}

impl Ctx {
    pub(crate) fn send(&self, m: ToTimer) {
        self.queue.push(m);
        self.thread.thread().unpark();
    }
}

pub(crate) static CTX: LazyLock<Ctx> = LazyLock::new(|| {
    let queue = Arc::new(SegQueue::new());
    let thread = thread::spawn({
        let queue = queue.clone();
        move || run(queue)
    });
    Ctx { queue, thread }
});

enum TimerKind {
    Once(unit_channel::Sender),
    Interval {
        period: Duration,
        signal: unit_channel::Sender,
    },
}

const GRAN: Duration = Duration::from_micros(10);

struct ThreadCtx {
    queue: Arc<SegQueue<ToTimer>>,
    pending: BTreeMap<Instant, SmallVec<[TimerKind; 1]>>,
    by_id: FxHashMap<Id, Instant>,
}

impl ThreadCtx {
    fn register_interval(
        &mut self,
        start: Instant,
        period: Duration,
        signal: unit_channel::Sender,
    ) {
        let id = signal.id();
        self.pending
            .entry(start)
            .or_insert_with(|| smallvec![])
            .push(TimerKind::Interval { period, signal });
        self.by_id.insert(id, start);
    }

    fn register_once(&mut self, when: Instant, signal: unit_channel::Sender) {
        let id = signal.id();
        if let Some(k) = self.by_id.remove(&id) {
            if let Entry::Occupied(mut e) = self.pending.entry(k) {
                e.get_mut().retain(|v| match v {
                    TimerKind::Once(signal) => signal.id() != id,
                    TimerKind::Interval { .. } => false,
                });
                if e.get().is_empty() {
                    e.remove();
                }
            }
        }
        self.pending
            .entry(when)
            .or_insert_with(|| smallvec![])
            .push(TimerKind::Once(signal));
        self.by_id.insert(id, when);
    }

    fn process_queue(&mut self) {
        while let Some(m) = self.queue.pop() {
            match m {
                ToTimer::Once { deadline, signal } => self.register_once(deadline, signal),
                ToTimer::Interval {
                    start,
                    period,
                    signal,
                } => self.register_interval(start, period, signal),
                ToTimer::CancelInterval(to_cancel) => {
                    if let Some(k) = self.by_id.remove(&to_cancel) {
                        if let Entry::Occupied(mut e) = self.pending.entry(k) {
                            e.get_mut().retain(|v| match v {
                                TimerKind::Once(_) => true,
                                TimerKind::Interval { signal, .. } => signal.id() != to_cancel,
                            });
                            if e.get().is_empty() {
                                e.remove();
                            }
                        }
                    }
                }
            }
        }
    }
}

fn run(queue: Arc<SegQueue<ToTimer>>) {
    let mut ctx = ThreadCtx {
        queue,
        pending: BTreeMap::new(),
        by_id: FxHashMap::default(),
    };
    loop {
        ctx.process_queue();
        let now = Instant::now();
        match ctx.pending.first_key_value().map(|kv| *kv.0) {
            None => thread::park(),
            Some(next) => {
                if next - now > GRAN {
                    thread::park_timeout(next - now - GRAN)
                } else {
                    for t in ctx.pending.pop_first().unwrap().1 {
                        match t {
                            TimerKind::Once(signal) => {
                                signal.send();
                            }
                            TimerKind::Interval { period, signal } => {
                                signal.send();
                                ctx.register_interval(now + period, period, signal);
                            }
                        }
                    }
                }
            }
        }
    }
}
