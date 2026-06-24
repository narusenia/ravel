// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fixed-size thread pool with bounded job queue for media decode work.
//!
//! Uses `std::thread` (not rayon) so long-running FFmpeg calls cannot starve
//! the evaluation pool. The bounded channel provides backpressure: submitting
//! a job blocks when the queue is full.

use crossbeam_channel::{Sender, TrySendError, bounded};
use std::num::NonZeroUsize;
use std::thread::{self, JoinHandle};

/// Configuration for the decode pool.
#[derive(Clone, Debug)]
pub struct DecodePoolConfig {
    /// Number of worker threads (default: 2).
    pub num_workers: NonZeroUsize,
    /// Maximum number of pending jobs before backpressure kicks in (default: 32).
    pub queue_capacity: usize,
}

impl Default for DecodePoolConfig {
    fn default() -> Self {
        Self {
            num_workers: NonZeroUsize::new(2).unwrap(),
            queue_capacity: 32,
        }
    }
}

type Job = Box<dyn FnOnce() + Send + 'static>;

enum Message {
    Work(Job),
    Shutdown,
}

/// A fixed-size thread pool with a bounded job queue.
///
/// Workers pull jobs from a shared `crossbeam_channel::bounded` queue.
/// When the queue is full, [`submit`](DecodePool::submit) blocks (backpressure).
/// [`try_submit`](DecodePool::try_submit) returns an error instead.
pub struct DecodePool {
    sender: Option<Sender<Message>>,
    workers: Vec<JoinHandle<()>>,
}

impl DecodePool {
    pub fn new(config: DecodePoolConfig) -> Self {
        let n = config.num_workers.get();
        let (sender, receiver) = bounded::<Message>(config.queue_capacity);
        let mut workers = Vec::with_capacity(n);

        for i in 0..n {
            let rx = receiver.clone();
            let handle = thread::Builder::new()
                .name(format!("ravel-decode-{i}"))
                .spawn(move || {
                    let _span = tracing::info_span!("decode_worker", worker = i).entered();
                    while let Ok(msg) = rx.recv() {
                        match msg {
                            Message::Work(job) => job(),
                            Message::Shutdown => break,
                        }
                    }
                })
                .expect("failed to spawn decode worker");
            workers.push(handle);
        }

        tracing::info!(
            workers = n,
            queue = config.queue_capacity,
            "decode pool started"
        );

        Self {
            sender: Some(sender),
            workers,
        }
    }

    /// Submit a job, blocking if the queue is full.
    ///
    /// Returns `false` if the pool has been shut down or all workers panicked.
    pub fn submit<F>(&self, job: F) -> bool
    where
        F: FnOnce() + Send + 'static,
    {
        match &self.sender {
            Some(tx) => match tx.send(Message::Work(Box::new(job))) {
                Ok(()) => true,
                Err(_) => {
                    tracing::warn!("decode pool: job dropped (channel disconnected)");
                    false
                }
            },
            None => false,
        }
    }

    /// Try to submit a job without blocking. Returns `false` if the queue is
    /// full or the pool has been shut down.
    pub fn try_submit<F>(&self, job: F) -> bool
    where
        F: FnOnce() + Send + 'static,
    {
        match &self.sender {
            Some(tx) => match tx.try_send(Message::Work(Box::new(job))) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => false,
                Err(TrySendError::Disconnected(_)) => {
                    tracing::warn!("decode pool: job dropped (channel disconnected)");
                    false
                }
            },
            None => false,
        }
    }

    /// Number of worker threads.
    pub fn num_workers(&self) -> usize {
        self.workers.len()
    }

    /// Gracefully shut down all workers. Blocks until every thread exits.
    pub fn shutdown(mut self) {
        if let Some(tx) = self.sender.take() {
            for _ in &self.workers {
                let _ = tx.send(Message::Shutdown);
            }
            // Drop sender so workers blocked on recv() also unblock.
            drop(tx);
        }
        for handle in self.workers {
            let _ = handle.join();
        }
        tracing::info!("decode pool shut down");
    }
}
