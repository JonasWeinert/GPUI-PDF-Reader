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
    states: Arc<[Arc<CancellationState>]>,
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
            states: Arc::from([self.state.clone()]),
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
    /// Returns a token that is cancelled when either input token is cancelled.
    ///
    /// Composition is intentionally engine-neutral: a document session and an
    /// individual scheduled operation can each retain their own cancellation
    /// owner while engine work receives a single read-only token.
    pub fn combined(&self, other: &Self) -> Self {
        let mut states = Vec::with_capacity(self.states.len() + other.states.len());
        for state in self.states.iter().chain(other.states.iter()) {
            if !states.iter().any(|existing| Arc::ptr_eq(existing, state)) {
                states.push(state.clone());
            }
        }
        Self {
            states: states.into(),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.states
            .iter()
            .any(|state| state.cancelled.load(Ordering::Acquire))
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

    #[test]
    fn combined_token_is_cancelled_by_either_independent_source() {
        let session = CancellationSource::new();
        let operation = CancellationSource::new();
        let combined = session.token().combined(&operation.token());

        operation.cancel();
        assert!(combined.is_cancelled());
        assert!(!session.is_cancelled());

        let session = CancellationSource::new();
        let operation = CancellationSource::new();
        let combined = session.token().combined(&operation.token());
        session.cancel();
        assert!(combined.is_cancelled());
        assert!(!operation.is_cancelled());
    }

    #[test]
    fn combining_a_token_with_itself_preserves_one_cancellation_state() {
        let source = CancellationSource::new();
        let token = source.token();
        let combined = token.combined(&token);
        assert_eq!(combined.states.len(), 1);
        source.cancel();
        assert_eq!(combined.checkpoint(), Err(Cancelled));
    }
}
