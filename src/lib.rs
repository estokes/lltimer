mod ctx;
mod unit_channel;

use pin_project::pin_project;
use std::{
    error::Error,
    fmt,
    future::{Future, IntoFuture},
    pin::{pin, Pin},
    result::Result,
    task::{Context, Poll},
    time::Duration,
};
use tokio::time::Instant;
use unit_channel::Id;

enum ToTimer {
    Once {
        deadline: Instant,
        signal: unit_channel::Sender,
    },
    Interval {
        start: Instant,
        period: Duration,
        signal: unit_channel::Sender,
    },
    CancelInterval(Id),
}

pub struct Interval(unit_channel::Receiver);

impl Drop for Interval {
    fn drop(&mut self) {
        ctx::CTX.send(ToTimer::CancelInterval(self.0.id()))
    }
}

impl Interval {
    pub async fn tick(&mut self) -> Instant {
        (&mut self.0).await;
        Instant::now()
    }
}

#[pin_project]
pub struct Sleep {
    deadline: Instant,
    #[pin]
    signal: unit_channel::Receiver,
}

impl Sleep {
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn is_elapsed(&self) -> bool {
        self.signal.peek()
    }

    pub fn reset(&mut self, deadline: Instant) {
        let tx = self.signal.sender();
        self.deadline = deadline;
        ctx::CTX.send(ToTimer::Once {
            deadline,
            signal: tx,
        })
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().signal.poll(cx)
    }
}

pub fn sleep_until(deadline: Instant) -> Sleep {
    let (tx, rx) = unit_channel::channel();
    ctx::CTX.send(ToTimer::Once {
        deadline,
        signal: tx,
    });
    Sleep {
        deadline,
        signal: rx,
    }
}

pub fn sleep(duration: Duration) -> Sleep {
    sleep_until(Instant::now() + duration)
}

pub fn interval_at(start: Instant, period: Duration) -> Interval {
    let (tx, rx) = unit_channel::channel();
    ctx::CTX.send(ToTimer::Interval {
        start,
        period,
        signal: tx,
    });
    Interval(rx)
}

pub fn interval(period: Duration) -> Interval {
    interval_at(Instant::now(), period)
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
