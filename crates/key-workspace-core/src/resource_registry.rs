//! Stateful resource-participant reconciliation built on the pure allocator.

use crate::{
    ActivityLevel, AllocationPlan, ParticipantSnapshot, ResourceAllocation, ResourceCoordinator,
    ResourceParticipantId, ResourceProfile,
};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationChange {
    pub previous: Option<ResourceAllocation>,
    pub current: ResourceAllocation,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Reconciliation {
    pub plan: Option<AllocationPlan>,
    pub changed: Vec<AllocationChange>,
    pub removed: Vec<ResourceParticipantId>,
}

#[derive(Clone, Copy, Debug)]
struct ParticipantRecord {
    snapshot: ParticipantSnapshot,
    allocation: Option<ResourceAllocation>,
}

/// Process-level registry. Domain owners apply returned deltas themselves, so
/// this type never holds UI, engine, thread, or callback objects.
#[derive(Debug, Default)]
pub struct ResourceRegistry {
    participants: BTreeMap<ResourceParticipantId, ParticipantRecord>,
    removed: Vec<ResourceParticipantId>,
}

impl ResourceRegistry {
    pub fn upsert(&mut self, snapshot: ParticipantSnapshot) {
        self.participants
            .entry(snapshot.id)
            .and_modify(|record| record.snapshot = snapshot)
            .or_insert(ParticipantRecord {
                snapshot,
                allocation: None,
            });
    }

    pub fn set_activity(&mut self, id: ResourceParticipantId, activity: ActivityLevel) -> bool {
        let Some(record) = self.participants.get_mut(&id) else {
            return false;
        };
        let changed = record.snapshot.activity != activity;
        record.snapshot.activity = activity;
        changed
    }

    pub fn set_profile(&mut self, id: ResourceParticipantId, profile: ResourceProfile) -> bool {
        let Some(record) = self.participants.get_mut(&id) else {
            return false;
        };
        let changed = record.snapshot.profile != profile;
        record.snapshot.profile = profile;
        changed
    }

    pub fn remove(&mut self, id: ResourceParticipantId) -> bool {
        if self.participants.remove(&id).is_none() {
            return false;
        }
        self.removed.push(id);
        true
    }

    pub fn snapshot(&self, id: ResourceParticipantId) -> Option<ParticipantSnapshot> {
        self.participants.get(&id).map(|record| record.snapshot)
    }

    pub fn allocation(&self, id: ResourceParticipantId) -> Option<ResourceAllocation> {
        self.participants
            .get(&id)
            .and_then(|record| record.allocation)
    }

    pub fn reconcile(&mut self, coordinator: &ResourceCoordinator) -> Reconciliation {
        let snapshots = self
            .participants
            .values()
            .map(|record| record.snapshot)
            .collect::<Vec<_>>();
        let plan = coordinator.plan(&snapshots);
        let mut changed = Vec::new();
        for allocation in &plan.allocations {
            let record = self
                .participants
                .get_mut(&allocation.id)
                .expect("allocator returned only registered participants");
            if record.allocation != Some(*allocation) {
                changed.push(AllocationChange {
                    previous: record.allocation,
                    current: *allocation,
                });
                record.allocation = Some(*allocation);
            }
        }
        Reconciliation {
            plan: Some(plan),
            changed,
            removed: std::mem::take(&mut self.removed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ResourceAmount, ResourceMode, SystemResources};

    fn coordinator() -> ResourceCoordinator {
        ResourceCoordinator::new(
            ResourceMode::Balanced,
            SystemResources {
                physical_memory_bytes: 16 * 1024 * 1024 * 1024,
                logical_cpus: 8,
                low_power_mode: false,
            },
        )
    }

    fn participant(id: u64, activity: ActivityLevel) -> ParticipantSnapshot {
        ParticipantSnapshot {
            id: ResourceParticipantId::from_raw(id),
            activity,
            profile: ResourceProfile::new(
                ResourceAmount {
                    cpu_memory_bytes: 32,
                    gpu_memory_bytes: 16,
                    worker_slots: 0,
                    network_slots: 0,
                },
                ResourceAmount {
                    cpu_memory_bytes: 256,
                    gpu_memory_bytes: 128,
                    worker_slots: 2,
                    network_slots: 1,
                },
            ),
        }
    }

    #[test]
    fn reconciliation_reports_only_real_allocation_changes() {
        let mut registry = ResourceRegistry::default();
        registry.upsert(participant(1, ActivityLevel::ForegroundInteractive));
        let first = registry.reconcile(&coordinator());
        assert_eq!(first.changed.len(), 1);
        assert!(
            registry
                .allocation(ResourceParticipantId::from_raw(1))
                .is_some()
        );

        let stable = registry.reconcile(&coordinator());
        assert!(stable.changed.is_empty());

        assert!(
            registry.set_activity(ResourceParticipantId::from_raw(1), ActivityLevel::Suspended)
        );
        let suspended = registry.reconcile(&coordinator());
        assert_eq!(suspended.changed.len(), 1);
        assert_eq!(
            suspended.changed[0].current.amount,
            ResourceAmount::default()
        );
    }

    #[test]
    fn removal_is_reported_once_and_unknown_updates_are_safe() {
        let id = ResourceParticipantId::from_raw(7);
        let mut registry = ResourceRegistry::default();
        registry.upsert(participant(7, ActivityLevel::BackgroundWarm));
        registry.reconcile(&coordinator());
        assert!(registry.remove(id));
        assert!(!registry.remove(id));
        assert!(!registry.set_activity(id, ActivityLevel::ForegroundIdle));
        assert_eq!(registry.reconcile(&coordinator()).removed, vec![id]);
        assert!(registry.reconcile(&coordinator()).removed.is_empty());
    }

    #[test]
    fn upsert_preserves_previous_allocation_until_replanned() {
        let id = ResourceParticipantId::from_raw(9);
        let mut registry = ResourceRegistry::default();
        registry.upsert(participant(9, ActivityLevel::ForegroundIdle));
        registry.reconcile(&coordinator());
        let before = registry.allocation(id).unwrap();
        registry.upsert(participant(9, ActivityLevel::BackgroundCold));
        assert_eq!(registry.allocation(id), Some(before));
        assert_eq!(registry.reconcile(&coordinator()).changed.len(), 1);
    }
}
