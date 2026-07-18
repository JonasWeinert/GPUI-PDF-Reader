use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Debug)]
struct CancellationState {
    cancelled: AtomicBool,
}

/// Owner side of a cooperative cancellation pair.
#[derive(Clone, Debug)]
pub struct CancellationSource {
    state: Arc<CancellationState>,
}

/// Read-only cancellation capability passed into engine work.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    state: Arc<CancellationState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cancelled;

impl CancellationSource {
    pub fn new() -> Self {
        Self {
            state: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
            }),
        }
    }

    pub fn token(&self) -> CancellationToken {
        CancellationToken {
            state: self.state.clone(),
        }
    }

    /// Cancels current and future observers. Returns whether this call changed
    /// the state.
    pub fn cancel(&self) -> bool {
        !self.state.cancelled.swap(true, Ordering::AcqRel)
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }
}

impl Default for CancellationSource {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    pub fn checkpoint(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
}

impl fmt::Display for Cancelled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("operation cancelled")
    }
}

impl std::error::Error for Cancelled {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_shared_idempotent_and_read_only_to_workers() {
        let source = CancellationSource::new();
        let first = source.token();
        let second = first.clone();
        assert_eq!(first.checkpoint(), Ok(()));
        assert!(source.cancel());
        assert!(!source.cancel());
        assert_eq!(first.checkpoint(), Err(Cancelled));
        assert!(second.is_cancelled());
    }
}
