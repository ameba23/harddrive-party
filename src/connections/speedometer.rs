//! Measure transfer speed
//!
//! Copied from https://github.com/datrs/speedometer - but uses [std::time::Instant] instead of the
//! `instant` crate

use std::collections::vec_deque::VecDeque;
use std::time::{Duration, Instant};

/// Entries into the queue.
#[derive(Debug)]
pub struct Entry {
    timestamp: Instant,
    value: usize,
}

/// Measure speed in bytes/second.
#[derive(Debug)]
pub struct Speedometer {
    /// Size of the window over which we measure entries.
    pub window_size: Duration,
    queue: VecDeque<Entry>,
    total_value: usize,
}

impl Speedometer {
    /// Create a new instance.
    pub fn new(window_size: Duration) -> Self {
        Self {
            total_value: 0,
            queue: VecDeque::new(),
            window_size,
        }
    }

    /// Create a new instance with a queue of `capacity`.
    pub fn with_capacity(window_size: Duration, capacity: usize) -> Self {
        Self {
            total_value: 0,
            queue: VecDeque::with_capacity(capacity),
            window_size,
        }
    }

    /// Create a new instance with a new queue. Useful if you have prior knowledge
    /// of how big the allocation for the queue should be.
    pub fn with_queue(window_size: Duration, queue: VecDeque<Entry>) -> Self {
        assert!(queue.is_empty());
        Self {
            total_value: 0,
            queue,
            window_size,
        }
    }

    /// Enter a data point into the speedometer.
    pub fn entry(&mut self, value: usize) {
        self.total_value += value;
        self.queue.push_back(Entry {
            timestamp: Instant::now(),
            value,
        });
    }

    /// Measure the speed.
    pub fn measure(&mut self) -> usize {
        let mut max = 0;
        for (index, entry) in self.queue.iter_mut().enumerate() {
            if entry.timestamp.elapsed() > self.window_size {
                self.total_value -= entry.value;
            } else {
                max = index;
                break;
            }
        }

        for _ in 0..max {
            self.queue.pop_front();
        }

        if self.queue.is_empty() {
            0
        } else {
            self.total_value / self.queue.len()
        }
    }
}

impl Default for Speedometer {
    fn default() -> Self {
        Self {
            window_size: Duration::from_secs(5),
            total_value: 0,
            queue: VecDeque::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn measures_entries() {
        let window_size = Duration::from_secs(1);
        let mut meter = Speedometer::new(window_size);
        meter.entry(10);
        meter.entry(10);
        meter.entry(10);
        assert!(meter.measure() > 0, "bytes per second should be non-zero");
        std::thread::sleep(window_size);
        assert_eq!(meter.measure(), 0);
    }

    #[test]
    fn no_entries() {
        let window_size = Duration::from_secs(1);
        let mut meter = Speedometer::new(window_size);
        assert_eq!(meter.measure(), 0, "should not crash on empty queue");
    }
}
