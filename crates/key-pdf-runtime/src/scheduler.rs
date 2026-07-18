use crate::DemandPriority;
use std::{collections::HashMap, fmt, hash::Hash};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DemandRevision(u128);

impl DemandRevision {
    pub fn get(self) -> u128 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledDemand<K, V> {
    key: K,
    revision: DemandRevision,
    priority: DemandPriority,
    value: V,
}

impl<K, V> ScheduledDemand<K, V> {
    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn revision(&self) -> DemandRevision {
        self.revision
    }

    pub fn priority(&self) -> DemandPriority {
        self.priority
    }

    pub fn value(&self) -> &V {
        &self.value
    }

    pub fn into_value(self) -> V {
        self.value
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScheduleOutcome<K, V> {
    Queued {
        revision: DemandRevision,
    },
    Replaced {
        revision: DemandRevision,
        previous: V,
    },
    Evicted {
        revision: DemandRevision,
        evicted: ScheduledDemand<K, V>,
    },
    Rejected {
        value: V,
    },
}

impl<K, V> ScheduleOutcome<K, V> {
    pub fn revision(&self) -> Option<DemandRevision> {
        match self {
            Self::Queued { revision }
            | Self::Replaced { revision, .. }
            | Self::Evicted { revision, .. } => Some(*revision),
            Self::Rejected { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionDisposition {
    Publish,
    Stale,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulerError;

impl fmt::Display for SchedulerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("maximum in-flight demand count must be non-zero")
    }
}

impl std::error::Error for SchedulerError {}

#[derive(Clone, Debug)]
struct Queued<V> {
    revision: DemandRevision,
    priority: DemandPriority,
    value: V,
}

/// A bounded de-duplicating scheduler. New work replaces pending work with the
/// same key. At capacity, equal-or-higher-priority new work evicts the oldest
/// lowest-priority item. Completion publication is separately gated so an
/// in-flight result is suppressed when a newer demand arrives.
#[derive(Clone, Debug)]
pub struct LatestWinsQueue<K, V> {
    capacity: usize,
    max_in_flight: usize,
    next_revision: u128,
    queued: HashMap<K, Queued<V>>,
    in_flight: HashMap<K, DemandRevision>,
    latest: HashMap<K, DemandRevision>,
}

impl<K, V> LatestWinsQueue<K, V>
where
    K: Clone + Eq + Hash,
{
    pub fn new(capacity: usize) -> Self {
        Self::with_max_in_flight(capacity, 1).expect("one in-flight demand is valid")
    }

    pub fn with_max_in_flight(
        capacity: usize,
        max_in_flight: usize,
    ) -> Result<Self, SchedulerError> {
        if max_in_flight == 0 {
            return Err(SchedulerError);
        }
        Ok(Self {
            capacity,
            max_in_flight,
            next_revision: 1,
            queued: HashMap::with_capacity(capacity),
            in_flight: HashMap::with_capacity(max_in_flight),
            latest: HashMap::with_capacity(capacity.saturating_add(max_in_flight)),
        })
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.queued.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.queued.contains_key(key)
    }

    pub fn schedule(
        &mut self,
        key: K,
        priority: DemandPriority,
        value: V,
    ) -> ScheduleOutcome<K, V> {
        if self.capacity == 0 {
            return ScheduleOutcome::Rejected { value };
        }

        if let Some(previous) = self.queued.remove(&key) {
            let revision = self.allocate_revision();
            self.queued.insert(
                key.clone(),
                Queued {
                    revision,
                    priority,
                    value,
                },
            );
            self.latest.insert(key, revision);
            return ScheduleOutcome::Replaced {
                revision,
                previous: previous.value,
            };
        }

        let evicted = if self.queued.len() >= self.capacity {
            let eviction_key = self
                .queued
                .iter()
                .min_by_key(|(_, queued)| (queued.priority, queued.revision))
                .map(|(key, _)| key.clone())
                .expect("a full non-zero-capacity queue has an eviction candidate");
            let candidate = &self.queued[&eviction_key];
            if priority < candidate.priority {
                return ScheduleOutcome::Rejected { value };
            }
            let queued = self.queued.remove(&eviction_key).unwrap();
            if self.latest.get(&eviction_key) == Some(&queued.revision) {
                self.latest.remove(&eviction_key);
            }
            Some(ScheduledDemand {
                key: eviction_key,
                revision: queued.revision,
                priority: queued.priority,
                value: queued.value,
            })
        } else {
            None
        };

        let revision = self.allocate_revision();
        self.queued.insert(
            key.clone(),
            Queued {
                revision,
                priority,
                value,
            },
        );
        self.latest.insert(key, revision);
        match evicted {
            Some(evicted) => ScheduleOutcome::Evicted { revision, evicted },
            None => ScheduleOutcome::Queued { revision },
        }
    }

    /// Takes the highest-priority pending item; equal priorities favor the
    /// newest revision so fast viewport changes converge immediately.
    pub fn pop_next(&mut self) -> Option<ScheduledDemand<K, V>> {
        if self.in_flight.len() >= self.max_in_flight {
            return None;
        }
        let key = self
            .queued
            .iter()
            .filter(|(key, _)| !self.in_flight.contains_key(*key))
            .max_by_key(|(_, queued)| (queued.priority, queued.revision))
            .map(|(key, _)| key.clone())?;
        let queued = self.queued.remove(&key).unwrap();
        self.in_flight.insert(key.clone(), queued.revision);
        Some(ScheduledDemand {
            key,
            revision: queued.revision,
            priority: queued.priority,
            value: queued.value,
        })
    }

    /// Completes an in-flight item and decides whether its output still
    /// represents the latest accepted demand for that key.
    pub fn finish(&mut self, demand: &ScheduledDemand<K, V>) -> CompletionDisposition {
        if self.in_flight.get(&demand.key) != Some(&demand.revision) {
            return CompletionDisposition::Unknown;
        }
        self.in_flight.remove(&demand.key);
        if self.latest.get(&demand.key) == Some(&demand.revision) {
            self.latest.remove(&demand.key);
            CompletionDisposition::Publish
        } else {
            CompletionDisposition::Stale
        }
    }

    /// Cancels pending work and invalidates an in-flight completion with this
    /// key. The engine still owns cooperative cancellation of the operation.
    pub fn cancel(&mut self, key: &K) -> Option<V> {
        self.latest.remove(key);
        self.queued.remove(key).map(|queued| queued.value)
    }

    /// Replaces an entire pending demand set. In-flight work becomes stale;
    /// callers can then schedule the new viewport in priority order.
    pub fn clear_pending(&mut self) -> Vec<V> {
        self.latest.clear();
        self.queued
            .drain()
            .map(|(_, queued)| queued.value)
            .collect()
    }

    pub fn is_latest(&self, demand: &ScheduledDemand<K, V>) -> bool {
        self.latest.get(&demand.key) == Some(&demand.revision)
    }

    fn allocate_revision(&mut self) -> DemandRevision {
        let revision = DemandRevision(self.next_revision);
        self.next_revision = self
            .next_revision
            .checked_add(1)
            .expect("u128 demand revision space exhausted");
        revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_and_bounded_eviction_are_deterministic() {
        let mut queue = LatestWinsQueue::new(2);
        let first = queue.schedule("a", DemandPriority::VISIBLE, 1);
        let replaced = queue.schedule("a", DemandPriority::PREFETCH, 2);
        assert!(replaced.revision().unwrap() > first.revision().unwrap());
        assert_eq!(
            replaced,
            ScheduleOutcome::Replaced {
                revision: replaced.revision().unwrap(),
                previous: 1,
            }
        );
        queue.schedule("b", DemandPriority::BACKGROUND, 3);
        let evicted = queue.schedule("c", DemandPriority::BACKGROUND, 4);
        assert!(matches!(
            evicted,
            ScheduleOutcome::Evicted { evicted, .. } if evicted.key() == &"b" && evicted.value() == &3
        ));
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn lower_priority_work_cannot_displace_a_full_visible_queue() {
        let mut queue = LatestWinsQueue::new(1);
        queue.schedule("visible", DemandPriority::VISIBLE, 1);
        assert_eq!(
            queue.schedule("background", DemandPriority::BACKGROUND, 2),
            ScheduleOutcome::Rejected { value: 2 }
        );
        assert!(queue.contains_key(&"visible"));
    }

    #[test]
    fn newer_same_key_demand_suppresses_an_in_flight_completion() {
        let mut queue = LatestWinsQueue::new(4);
        queue.schedule("tile", DemandPriority::VISIBLE, "old");
        let old = queue.pop_next().unwrap();
        assert!(queue.is_latest(&old));
        queue.schedule("tile", DemandPriority::VISIBLE, "new");
        assert!(!queue.is_latest(&old));
        assert_eq!(queue.finish(&old), CompletionDisposition::Stale);

        let new = queue.pop_next().unwrap();
        assert_eq!(new.value(), &"new");
        assert_eq!(queue.finish(&new), CompletionDisposition::Publish);
    }

    #[test]
    fn clear_pending_invalidates_in_flight_and_latest_equal_priority_runs_first() {
        let mut queue = LatestWinsQueue::new(4);
        queue.schedule(1, DemandPriority::VISIBLE, "first");
        queue.schedule(2, DemandPriority::VISIBLE, "latest");
        let latest = queue.pop_next().unwrap();
        assert_eq!(latest.value(), &"latest");
        queue.clear_pending();
        assert_eq!(queue.finish(&latest), CompletionDisposition::Stale);
    }

    #[test]
    fn the_same_key_is_never_executed_concurrently() {
        let mut queue = LatestWinsQueue::with_max_in_flight(4, 2).unwrap();
        queue.schedule("tile", DemandPriority::VISIBLE, "old");
        let old = queue.pop_next().unwrap();
        queue.schedule("tile", DemandPriority::VISIBLE, "new");
        assert!(queue.pop_next().is_none());
        assert_eq!(queue.finish(&old), CompletionDisposition::Stale);
        let new = queue.pop_next().unwrap();
        assert_eq!(new.value(), &"new");
        assert_eq!(queue.finish(&new), CompletionDisposition::Publish);
    }
}
