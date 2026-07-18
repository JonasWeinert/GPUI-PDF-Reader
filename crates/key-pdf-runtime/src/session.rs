use crate::{CancellationSource, CancellationToken};
use std::{
    fmt,
    marker::PhantomData,
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DocumentGeneration(NonZeroU64);

impl DocumentGeneration {
    pub fn get(self) -> u64 {
        self.0.get()
    }

    pub fn from_non_zero(value: NonZeroU64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestId(NonZeroU64);

impl RequestId {
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceId(NonZeroU64);

impl ResourceId {
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// Opaque resource identity whose type and document generation are checked at
/// compile time and at the host boundary respectively.
pub struct ResourceHandle<T> {
    generation: DocumentGeneration,
    id: ResourceId,
    marker: PhantomData<fn() -> T>,
}

impl<T> fmt::Debug for ResourceHandle<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceHandle")
            .field("generation", &self.generation)
            .field("id", &self.id)
            .finish()
    }
}

impl<T> ResourceHandle<T> {
    pub fn generation(self) -> DocumentGeneration {
        self.generation
    }

    pub fn id(self) -> ResourceId {
        self.id
    }

    pub fn belongs_to(self, generation: DocumentGeneration) -> bool {
        self.generation == generation
    }
}

impl<T> Clone for ResourceHandle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for ResourceHandle<T> {}

impl<T> PartialEq for ResourceHandle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && self.id == other.id
    }
}

impl<T> Eq for ResourceHandle<T> {}

impl<T> std::hash::Hash for ResourceHandle<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.generation.hash(state);
        self.id.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllocationError {
    RequestIdsExhausted,
    ResourceIdsExhausted,
}

impl fmt::Display for AllocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequestIdsExhausted => formatter.write_str("document request IDs exhausted"),
            Self::ResourceIdsExhausted => formatter.write_str("document resource IDs exhausted"),
        }
    }
}

impl std::error::Error for AllocationError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionError {
    GenerationsExhausted,
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("document generations exhausted")
    }
}

impl std::error::Error for SessionError {}

#[derive(Debug)]
struct SessionState {
    generation: DocumentGeneration,
    cancellation: CancellationSource,
    next_request: AtomicU64,
    next_resource: AtomicU64,
}

/// Cloneable capability for one open-document attempt. Allocations are safe
/// across threads and never escape their generation.
#[derive(Clone, Debug)]
pub struct DocumentSession {
    state: Arc<SessionState>,
}

impl DocumentSession {
    fn new(generation: DocumentGeneration) -> Self {
        Self {
            state: Arc::new(SessionState {
                generation,
                cancellation: CancellationSource::new(),
                next_request: AtomicU64::new(1),
                next_resource: AtomicU64::new(1),
            }),
        }
    }

    pub fn generation(&self) -> DocumentGeneration {
        self.state.generation
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.state.cancellation.token()
    }

    pub fn cancel(&self) -> bool {
        self.state.cancellation.cancel()
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.cancellation.is_cancelled()
    }

    pub fn next_request_id(&self) -> Result<RequestId, AllocationError> {
        allocate_non_zero(&self.state.next_request)
            .map(RequestId)
            .ok_or(AllocationError::RequestIdsExhausted)
    }

    pub fn allocate_resource<T>(&self) -> Result<ResourceHandle<T>, AllocationError> {
        let id = allocate_non_zero(&self.state.next_resource)
            .map(ResourceId)
            .ok_or(AllocationError::ResourceIdsExhausted)?;
        Ok(ResourceHandle {
            generation: self.generation(),
            id,
            marker: PhantomData,
        })
    }

    pub fn owns<T>(&self, handle: ResourceHandle<T>) -> bool {
        handle.belongs_to(self.generation())
    }
}

fn allocate_non_zero(counter: &AtomicU64) -> Option<NonZeroU64> {
    let value = counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .ok()?;
    NonZeroU64::new(value)
}

/// Owns the current session and invalidates it before starting another.
#[derive(Debug)]
pub struct DocumentSessionManager {
    next_generation: u64,
    active: Option<DocumentSession>,
}

impl DocumentSessionManager {
    pub fn new() -> Self {
        Self {
            next_generation: 1,
            active: None,
        }
    }

    pub fn begin(&mut self) -> Result<DocumentSession, SessionError> {
        let generation = NonZeroU64::new(self.next_generation)
            .map(DocumentGeneration)
            .ok_or(SessionError::GenerationsExhausted)?;
        self.next_generation = self.next_generation.checked_add(1).unwrap_or(0);
        self.close();
        let session = DocumentSession::new(generation);
        self.active = Some(session.clone());
        Ok(session)
    }

    pub fn current(&self) -> Option<&DocumentSession> {
        self.active.as_ref()
    }

    pub fn is_current(&self, generation: DocumentGeneration) -> bool {
        self.active
            .as_ref()
            .is_some_and(|session| session.generation() == generation && !session.is_cancelled())
    }

    pub fn close(&mut self) -> Option<DocumentGeneration> {
        let session = self.active.take()?;
        session.cancel();
        Some(session.generation())
    }
}

impl Default for DocumentSessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum TestResource {}

    #[test]
    fn starting_a_session_cancels_and_invalidates_the_previous_generation() {
        let mut manager = DocumentSessionManager::new();
        let first = manager.begin().unwrap();
        let request = first.next_request_id().unwrap();
        let handle = first.allocate_resource::<TestResource>().unwrap();
        assert_eq!(request.get(), 1);
        assert!(first.owns(handle));
        assert!(manager.is_current(first.generation()));

        let second = manager.begin().unwrap();
        assert!(first.is_cancelled());
        assert!(!manager.is_current(first.generation()));
        assert!(manager.is_current(second.generation()));
        assert!(!second.owns(handle));
        assert_ne!(first.generation(), second.generation());
    }

    #[test]
    fn close_cancels_all_clones() {
        let mut manager = DocumentSessionManager::new();
        let session = manager.begin().unwrap();
        let clone = session.clone();
        assert_eq!(manager.close(), Some(session.generation()));
        assert!(clone.is_cancelled());
        assert!(manager.current().is_none());
    }
}
