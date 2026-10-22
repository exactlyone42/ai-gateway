use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub struct ProcessLifecycle {
    inner: Arc<LifecycleInner>,
}

#[derive(Default)]
struct LifecycleInner {
    draining: AtomicBool,
    draining_started: Notify,
    active: AtomicUsize,
    idle: Notify,
    force: CancellationToken,
}

impl ProcessLifecycle {
    pub fn admit(&self) -> Option<ActiveRequest> {
        if self.is_draining() {
            return None;
        }
        self.inner.active.fetch_add(1, Ordering::SeqCst);
        if self.is_draining() {
            release(&self.inner);
            return None;
        }
        Some(ActiveRequest {
            _lease: Arc::new(ActiveLease {
                lifecycle: Arc::clone(&self.inner),
            }),
        })
    }

    pub fn begin_draining(&self) -> usize {
        self.inner.draining.store(true, Ordering::SeqCst);
        self.inner.draining_started.notify_waiters();
        self.active()
    }

    pub fn is_draining(&self) -> bool {
        self.inner.draining.load(Ordering::SeqCst)
    }

    pub fn active(&self) -> usize {
        self.inner.active.load(Ordering::SeqCst)
    }

    pub async fn wait_for_idle(&self) {
        loop {
            let notified = self.inner.idle.notified();
            if self.active() == 0 {
                return;
            }
            notified.await;
        }
    }

    pub async fn wait_for_draining(&self) {
        loop {
            let notified = self.inner.draining_started.notified();
            if self.is_draining() {
                return;
            }
            notified.await;
        }
    }

    pub fn force(&self) {
        self.inner.force.cancel();
    }

    pub fn force_token(&self) -> CancellationToken {
        self.inner.force.clone()
    }
}

#[derive(Clone)]
pub struct ActiveRequest {
    _lease: Arc<ActiveLease>,
}

struct ActiveLease {
    lifecycle: Arc<LifecycleInner>,
}

impl Drop for ActiveLease {
    fn drop(&mut self) {
        release(&self.lifecycle);
    }
}

fn release(lifecycle: &LifecycleInner) {
    let previous = lifecycle.active.fetch_sub(1, Ordering::SeqCst);
    if previous == 1 {
        lifecycle.idle.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessLifecycle;

    #[tokio::test]
    async fn draining_rejects_new_work_and_existing_work_can_finish() {
        let lifecycle = ProcessLifecycle::default();
        let active = lifecycle.admit().expect("initial request was rejected");
        assert_eq!(lifecycle.begin_draining(), 1);
        assert!(lifecycle.admit().is_none());
        drop(active);
        lifecycle.wait_for_idle().await;
        assert_eq!(lifecycle.active(), 0);
    }
}
