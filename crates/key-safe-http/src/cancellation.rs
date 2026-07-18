use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Creates and owns the authority to cancel an HTTP operation.
#[derive(Clone, Debug, Default)]
pub struct CancellationSource {
    cancelled: Arc<AtomicBool>,
}

impl CancellationSource {
    /// Creates a new, initially active cancellation source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the read-only token passed to an operation.
    #[must_use]
    pub fn token(&self) -> CancellationToken {
        CancellationToken {
            cancelled: Arc::clone(&self.cancelled),
        }
    }

    /// Cancels every token created by this source.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Reports whether cancellation has already been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// A cheap, cloneable cancellation signal.
///
/// Cancellation is cooperative. The safe client checks it before DNS, before
/// each transport hop, and while streaming the response. A transport also
/// receives the token and should interrupt its own blocking work when its
/// implementation permits that.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Returns a token that has not been cancelled and has no public canceller.
    #[must_use]
    pub fn active() -> Self {
        Self::default()
    }

    /// Reports whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_cancels_all_derived_tokens() {
        let source = CancellationSource::new();
        let first = source.token();
        let second = first.clone();
        assert!(!first.is_cancelled());

        source.cancel();

        assert!(first.is_cancelled());
        assert!(second.is_cancelled());
        assert!(source.is_cancelled());
    }
}
