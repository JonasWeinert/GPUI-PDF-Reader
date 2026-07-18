use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! numeric_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub const fn from_raw(value: u64) -> Self {
                Self(value)
            }

            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

numeric_id!(WindowId);
numeric_id!(ItemId);
numeric_id!(TabId);
numeric_id!(ViewId);
numeric_id!(ResourceParticipantId);
numeric_id!(WorkId);

/// Monotonic invalidation token for work associated with an item or view.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct Generation(u64);

impl Generation {
    pub const INITIAL: Self = Self(0);

    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    pub const fn is_current(self, current: Self) -> bool {
        self.0 == current.0
    }
}

/// Process-local generator. Persisted identities should be supplied through
/// `from_raw` by the persistence layer instead.
#[derive(Debug, Default)]
pub struct IdGenerator {
    next: AtomicU64,
}

impl IdGenerator {
    pub const fn new(first: u64) -> Self {
        Self {
            next: AtomicU64::new(first),
        }
    }

    fn take(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    pub fn window(&self) -> WindowId {
        WindowId::from_raw(self.take())
    }

    pub fn item(&self) -> ItemId {
        ItemId::from_raw(self.take())
    }

    pub fn tab(&self) -> TabId {
        TabId::from_raw(self.take())
    }

    pub fn view(&self) -> ViewId {
        ViewId::from_raw(self.take())
    }

    pub fn participant(&self) -> ResourceParticipantId {
        ResourceParticipantId::from_raw(self.take())
    }

    pub fn work(&self) -> WorkId {
        WorkId::from_raw(self.take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generators_never_reuse_ids_across_kinds() {
        let ids = IdGenerator::new(7);
        assert_eq!(ids.window().get(), 7);
        assert_eq!(ids.item().get(), 8);
        assert_eq!(ids.tab().get(), 9);
        assert_eq!(ids.view().get(), 10);
    }

    #[test]
    fn generations_are_saturating_invalidation_tokens() {
        assert!(Generation::INITIAL.is_current(Generation::from_raw(0)));
        assert_eq!(Generation::from_raw(u64::MAX).next().get(), u64::MAX);
    }
}
