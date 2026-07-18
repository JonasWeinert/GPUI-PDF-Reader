use crate::{Generation, ResourceParticipantId, WorkId};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerDomain {
    PdfEngine,
    RenderUpload,
    Persistence,
    Network,
    Extension,
    MediaDecode,
    GeneralCpu,
    Custom(String),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionConstraint {
    MainThread,
    SerializedProcessWide,
    BoundedParallel { max_in_flight: u16 },
    Cooperative,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkPriority {
    Maintenance,
    Background,
    VisiblePrefetch,
    UserInitiated,
    Interactive,
    Critical,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkCost {
    pub cpu_micros: u64,
    pub memory_bytes: u64,
    pub io_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkRequest {
    pub id: WorkId,
    pub owner: ResourceParticipantId,
    pub generation: Generation,
    pub domain: SchedulerDomain,
    pub priority: WorkPriority,
    pub estimated_cost: WorkCost,
    pub cancellable: bool,
}

impl WorkRequest {
    pub fn is_stale(&self, current_generation: Generation) -> bool {
        !self.generation.is_current(current_generation)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DomainBudget {
    pub max_in_flight: u16,
    pub max_memory_bytes: u64,
    pub cooperative_time_slice: Duration,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DomainPolicy {
    pub domain: SchedulerDomain,
    pub constraint: ExecutionConstraint,
    pub budget: DomainBudget,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_work_is_identified_before_dispatch() {
        let request = WorkRequest {
            id: WorkId::from_raw(1),
            owner: ResourceParticipantId::from_raw(2),
            generation: Generation::from_raw(3),
            domain: SchedulerDomain::PdfEngine,
            priority: WorkPriority::Interactive,
            estimated_cost: WorkCost::default(),
            cancellable: true,
        };
        assert!(request.is_stale(Generation::from_raw(4)));
        assert!(!request.is_stale(Generation::from_raw(3)));
    }
}
