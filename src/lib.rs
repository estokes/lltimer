use crossbeam::{atomic::AtomicCell, queue::SegQueue};
use fxhash::FxHashMap;
use pin_project::pin_project;
use smallvec::{smallvec, SmallVec};
use std::{
    collections::{btree_map::Entry, BTreeMap},
    error::Error,
    fmt,
    future::{poll_fn, Future, IntoFuture},
    pin::{pin, Pin},
    result::Result,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, LazyLock,
    },
    task::{Context, Poll},
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::{sync::oneshot, time::Instant};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Id(usize);

impl Default for Id {
    fn default() -> Self {
        static CUR: AtomicUsize = AtomicUsize::new(0);
        Self(CUR.fetch_add(1, Ordering::Relaxed))
    }
}

enum ToTimer {
    Once {
        when: Instant,
        signal: oneshot::Sender<()>,
    },
    Interval {
        id: Id,
        interval: Duration,
        tick: Arc<AtomicCell<Option<oneshot::Sender<Instant>>>>,
    },
    CancelInterval(Id),
}

pub struct Sleep(oneshot::Receiver<()>);

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0)
            .poll(cx)
            .map(|r| r.unwrap_or_else(|_| ()))
    }
}

pub struct Interval {
    id: Id,
    tick: oneshot::Receiver<Instant>,
    register: Arc<AtomicCell<Option<oneshot::Sender<Instant>>>>,
}

impl Drop for Interval {
    fn drop(&mut self) {
        CTX.queue.push(ToTimer::CancelInterval(self.id));
        CTX.thread.thread().unpark()
    }
}

impl Interval {
    pub async fn tick(&mut self) -> Instant {
        poll_fn(|cx| {
            Pin::new(&mut self.tick).poll(cx).map(|r| match r {
                Err(_) => None,
                Ok(i) => {
                    let (tx, rx) = oneshot::channel();
                    self.tick = rx;
                    self.register.store(Some(tx));
                    Some(i)
                }
            })
        })
        .await
        .unwrap_or_else(|| Instant::now())
    }
}

struct TimerCtx {
    queue: Arc<SegQueue<ToTimer>>,
    thread: JoinHandle<()>,
}

static CTX: LazyLock<TimerCtx> = LazyLock::new(init);

fn init() -> TimerCtx {
    let queue = Arc::new(SegQueue::new());
    let thread = thread::spawn({
        let queue = queue.clone();
        move || TimerThreadCtx::run(queue)
    });
    TimerCtx { queue, thread }
}

enum TimerKind {
    Once(oneshot::Sender<()>),
    Interval {
        id: Id,
        interval: Duration,
        tick: Arc<AtomicCell<Option<oneshot::Sender<Instant>>>>,
    },
}

const GRAN: Duration = Duration::from_micros(10);

struct TimerThreadCtx {
    queue: Arc<SegQueue<ToTimer>>,
    pending: BTreeMap<Instant, SmallVec<[TimerKind; 1]>>,
    by_id: FxHashMap<Id, Instant>,
}

impl TimerThreadCtx {
    fn register_interval(
        &mut self,
        now: Instant,
        id: Id,
        interval: Duration,
        tick: Arc<AtomicCell<Option<oneshot::Sender<Instant>>>>,
    ) {
        let k = now + interval;
        self.pending
            .entry(k)
            .or_insert_with(|| smallvec![])
            .push(TimerKind::Interval { id, interval, tick });
        self.by_id.insert(id, k);
    }

    fn register_once(&mut self, now: Instant, when: Instant, signal: oneshot::Sender<()>) {
        if when <= now {
            let _ = signal.send(());
        } else {
            self.pending
                .entry(when)
                .or_insert_with(|| smallvec![])
                .push(TimerKind::Once(signal));
        }
    }

    fn process_queue(&mut self, now: Instant) {
        while let Some(m) = self.queue.pop() {
            match m {
                ToTimer::Once { when, signal } => self.register_once(now, when, signal),
                ToTimer::Interval { id, interval, tick } => {
                    self.register_interval(now, id, interval, tick)
                }
                ToTimer::CancelInterval(to_cancel) => {
                    if let Some(k) = self.by_id.remove(&to_cancel) {
                        if let Entry::Occupied(mut e) = self.pending.entry(k) {
                            e.get_mut().retain(|v| match v {
                                TimerKind::Once { .. } => true,
                                TimerKind::Interval { id, .. } => id != &to_cancel,
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

    fn run(queue: Arc<SegQueue<ToTimer>>) {
        let mut ctx = TimerThreadCtx {
            queue,
            pending: BTreeMap::new(),
            by_id: FxHashMap::default(),
        };
        loop {
            let now = Instant::now();
            ctx.process_queue(now);
            match ctx.pending.first_key_value().map(|kv| *kv.0) {
                None => thread::park(),
                Some(next) => {
                    if next - now > GRAN {
                        thread::park_timeout(next - now - GRAN)
                    } else {
                        for t in ctx.pending.pop_first().unwrap().1 {
                            match t {
                                TimerKind::Once(signal) => {
                                    let _ = signal.send(());
                                }
                                TimerKind::Interval { id, interval, tick } => {
                                    if let Some(ch) = tick.take() {
                                        let _ = ch.send(now);
                                    }
                                    ctx.register_interval(now, id, interval, tick);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub fn sleep(duration: Duration) -> Sleep {
    let ctx = &*CTX;
    let (signal, rx) = oneshot::channel();
    ctx.queue.push(ToTimer::Once {
        when: Instant::now() + duration,
        signal,
    });
    ctx.thread.thread().unpark();
    Sleep(rx)
}

pub fn interval(interval: Duration) -> Interval {
    let ctx = &*CTX;
    let id = Id::default();
    let (tx, tick) = oneshot::channel();
    let register = Arc::new(AtomicCell::new(Some(tx)));
    ctx.queue.push(ToTimer::Interval {
        id,
        interval,
        tick: register.clone(),
    });
    ctx.thread.thread().unpark();
    Interval { id, tick, register }
}

#[derive(Debug, Clone, Copy)]
pub struct Elapsed;

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timeout expired")
    }
}

impl Error for Elapsed {}

#[pin_project]
pub struct Timeout<F> {
    #[pin]
    timeout: Sleep,
    #[pin]
    future: F,
}

impl<F: Future> Future for Timeout<F> {
    type Output = Result<<F as Future>::Output, Elapsed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let proj = self.project();
        match proj.future.poll(cx) {
            Poll::Ready(r) => Poll::Ready(Ok(r)),
            Poll::Pending => match proj.timeout.poll(cx) {
                Poll::Ready(_) => Poll::Ready(Err(Elapsed)),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

pub fn timeout<F: IntoFuture>(duration: Duration, future: F) -> Timeout<F::IntoFuture> {
    Timeout {
        timeout: sleep(duration),
        future: future.into_future(),
    }
}
