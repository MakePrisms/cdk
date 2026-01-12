//! Periodic task supervisor for managing background cleanup jobs
//!
//! This module provides a generic supervisor for periodic background tasks that need
//! to run at regular intervals with proper lifecycle management.

use std::time::Duration;

use parking_lot::Mutex;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

/// Supervisor for periodic background tasks
pub struct PeriodicSupervisor {
    shutdown: CancellationToken,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl PeriodicSupervisor {
    /// Create a new periodic supervisor
    pub fn new() -> Self {
        Self {
            shutdown: CancellationToken::new(),
            handle: Mutex::new(None),
        }
    }

    /// Start the periodic task
    ///
    /// The task will run immediately and then repeats at the given interval.
    pub fn start<F, Fut>(&self, interval: Duration, mut task: F)
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut guard = self.handle.lock();
        if guard.is_some() {
            return;
        }

        let shutdown = self.shutdown.clone();
        let join_handle = tokio::spawn(async move {
            // Run immediately
            task().await;

            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        break;
                    }
                    _ = ticker.tick() => {
                        task().await;
                    }
                }
            }
        });

        *guard = Some(join_handle);
    }
}

impl Default for PeriodicSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PeriodicSupervisor {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(handle) = self.handle.lock().take() {
            handle.abort();
        }
    }
}
