use crate::ResourceParticipantId;
use serde::{Deserialize, Serialize};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceMode {
    Auto,
    Saver,
    Balanced,
    Performance,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityLevel {
    Suspended,
    BackgroundCold,
    BackgroundWarm,
    ForegroundIdle,
    ForegroundInteractive,
}

impl ActivityLevel {
    const fn weight(self) -> u64 {
        match self {
            Self::Suspended => 0,
            Self::BackgroundCold => 1,
            Self::BackgroundWarm => 2,
            Self::ForegroundIdle => 4,
            Self::ForegroundInteractive => 8,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceAmount {
    pub cpu_memory_bytes: u64,
    pub gpu_memory_bytes: u64,
    pub worker_slots: u64,
    pub network_slots: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceProfile {
    pub minimum: ResourceAmount,
    pub target: ResourceAmount,
    pub weight: u16,
    pub suspendable: bool,
}

impl ResourceProfile {
    pub fn new(minimum: ResourceAmount, target: ResourceAmount) -> Self {
        Self {
            minimum,
            target,
            weight: 1,
            suspendable: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParticipantSnapshot {
    pub id: ResourceParticipantId,
    pub activity: ActivityLevel,
    pub profile: ResourceProfile,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceAllocation {
    pub id: ResourceParticipantId,
    pub activity: ActivityLevel,
    pub amount: ResourceAmount,
}

/// A participant owns domain resources, while the coordinator only supplies
/// policy decisions. This keeps UI and backend lifecycles out of this crate.
pub trait ResourceParticipant {
    fn resource_snapshot(&self) -> ParticipantSnapshot;
    fn apply_resource_allocation(&mut self, allocation: ResourceAllocation);
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SystemResources {
    pub physical_memory_bytes: u64,
    pub logical_cpus: u16,
    pub low_power_mode: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AllocationPlan {
    pub requested_mode: ResourceMode,
    pub effective_mode: ResourceMode,
    pub budget: ResourceAmount,
    pub allocations: Vec<ResourceAllocation>,
}

#[derive(Clone, Copy, Debug)]
pub struct ResourceCoordinator {
    mode: ResourceMode,
    system: SystemResources,
}

impl ResourceCoordinator {
    pub fn new(mode: ResourceMode, system: SystemResources) -> Self {
        Self { mode, system }
    }

    pub const fn mode(&self) -> ResourceMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: ResourceMode) {
        self.mode = mode;
    }

    pub fn set_system_resources(&mut self, system: SystemResources) {
        self.system = system;
    }

    pub fn plan(&self, participants: &[ParticipantSnapshot]) -> AllocationPlan {
        let effective_mode = effective_mode(self.mode, self.system);
        let budget = budget_for(effective_mode, self.system);
        let mut allocations = participants
            .iter()
            .map(|participant| ResourceAllocation {
                id: participant.id,
                activity: participant.activity,
                amount: ResourceAmount::default(),
            })
            .collect::<Vec<_>>();

        allocate_dimension(
            participants,
            budget.cpu_memory_bytes,
            |amount| amount.cpu_memory_bytes,
            |amount, value| amount.cpu_memory_bytes = value,
            &mut allocations,
        );
        allocate_dimension(
            participants,
            budget.gpu_memory_bytes,
            |amount| amount.gpu_memory_bytes,
            |amount, value| amount.gpu_memory_bytes = value,
            &mut allocations,
        );
        allocate_dimension(
            participants,
            budget.worker_slots,
            |amount| amount.worker_slots,
            |amount, value| amount.worker_slots = value,
            &mut allocations,
        );
        allocate_dimension(
            participants,
            budget.network_slots,
            |amount| amount.network_slots,
            |amount, value| amount.network_slots = value,
            &mut allocations,
        );

        AllocationPlan {
            requested_mode: self.mode,
            effective_mode,
            budget,
            allocations,
        }
    }
}

fn effective_mode(mode: ResourceMode, system: SystemResources) -> ResourceMode {
    if mode != ResourceMode::Auto {
        return mode;
    }
    if system.low_power_mode || system.physical_memory_bytes <= 8 * GIB || system.logical_cpus <= 4
    {
        ResourceMode::Saver
    } else if system.physical_memory_bytes >= 24 * GIB && system.logical_cpus >= 8 {
        ResourceMode::Performance
    } else {
        ResourceMode::Balanced
    }
}

fn budget_for(mode: ResourceMode, system: SystemResources) -> ResourceAmount {
    let memory = system.physical_memory_bytes.max(2 * GIB);
    let cpus = u64::from(system.logical_cpus.max(1));
    match mode {
        ResourceMode::Auto => unreachable!("auto is resolved before budget calculation"),
        ResourceMode::Saver => ResourceAmount {
            cpu_memory_bytes: (memory / 10).clamp(192 * MIB, 2 * GIB),
            gpu_memory_bytes: (memory / 40).clamp(64 * MIB, 512 * MIB),
            worker_slots: (cpus / 4).max(1),
            network_slots: 2,
        },
        ResourceMode::Balanced => ResourceAmount {
            cpu_memory_bytes: (memory / 5).clamp(384 * MIB, 6 * GIB),
            gpu_memory_bytes: (memory / 20).clamp(128 * MIB, 1536 * MIB),
            worker_slots: (cpus / 2).max(1),
            network_slots: 4,
        },
        ResourceMode::Performance => ResourceAmount {
            cpu_memory_bytes: (memory * 35 / 100).clamp(768 * MIB, 12 * GIB),
            gpu_memory_bytes: (memory / 10).clamp(256 * MIB, 3 * GIB),
            worker_slots: cpus.saturating_sub(1).max(1),
            network_slots: 8,
        },
    }
}

fn allocate_dimension(
    participants: &[ParticipantSnapshot],
    budget: u64,
    get: impl Fn(ResourceAmount) -> u64 + Copy,
    set: impl Fn(&mut ResourceAmount, u64),
    output: &mut [ResourceAllocation],
) {
    if budget == 0 || participants.is_empty() {
        return;
    }

    let demands = participants
        .iter()
        .map(|participant| {
            let suspended =
                participant.activity == ActivityLevel::Suspended && participant.profile.suspendable;
            let minimum = if suspended {
                0
            } else {
                get(participant.profile.minimum)
            };
            let target = if suspended {
                0
            } else if participant.activity == ActivityLevel::Suspended {
                minimum
            } else {
                get(participant.profile.target).max(minimum)
            };
            let weight =
                participant.activity.weight() * u64::from(participant.profile.weight.max(1));
            (minimum, target, if suspended { 0 } else { weight.max(1) })
        })
        .collect::<Vec<_>>();

    let mut values = vec![0_u64; participants.len()];
    distribute(
        budget,
        &demands
            .iter()
            .map(|(minimum, _, _)| *minimum)
            .collect::<Vec<_>>(),
        &demands
            .iter()
            .map(|(_, _, weight)| *weight)
            .collect::<Vec<_>>(),
        &mut values,
    );

    let used = values.iter().sum::<u64>();
    if used < budget {
        let gaps = values
            .iter()
            .zip(&demands)
            .map(|(value, (_, target, _))| target.saturating_sub(*value))
            .collect::<Vec<_>>();
        let weights = demands
            .iter()
            .zip(&gaps)
            .map(|((_, _, weight), gap)| if *gap > 0 { *weight } else { 0 })
            .collect::<Vec<_>>();
        let mut extras = vec![0_u64; values.len()];
        distribute(budget - used, &gaps, &weights, &mut extras);
        for (value, extra) in values.iter_mut().zip(extras) {
            *value = value.saturating_add(extra);
        }
    }

    for (allocation, value) in output.iter_mut().zip(values) {
        set(&mut allocation.amount, value);
    }
}

/// Weighted capped distribution. It intentionally leaves unused capacity when
/// every cap is satisfied.
fn distribute(budget: u64, caps: &[u64], weights: &[u64], output: &mut [u64]) {
    let mut remaining = budget;
    let mut open = (0..caps.len())
        .filter(|index| caps[*index] > output[*index] && weights[*index] > 0)
        .collect::<Vec<_>>();

    while remaining > 0 && !open.is_empty() {
        let total_weight = open.iter().map(|index| weights[*index]).sum::<u64>();
        if total_weight == 0 {
            break;
        }

        let round_budget = remaining;
        let mut progressed = 0_u64;
        for index in open.iter().copied() {
            let capacity = caps[index].saturating_sub(output[index]);
            let share = ((round_budget as u128 * weights[index] as u128) / total_weight as u128)
                .max(1) as u64;
            let granted = capacity.min(share).min(remaining - progressed);
            output[index] = output[index].saturating_add(granted);
            progressed = progressed.saturating_add(granted);
            if progressed == remaining {
                break;
            }
        }
        if progressed == 0 {
            break;
        }
        remaining -= progressed;
        open.retain(|index| output[*index] < caps[*index]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system(memory_gib: u64, cpus: u16, low_power_mode: bool) -> SystemResources {
        SystemResources {
            physical_memory_bytes: memory_gib * GIB,
            logical_cpus: cpus,
            low_power_mode,
        }
    }

    fn participant(id: u64, activity: ActivityLevel) -> ParticipantSnapshot {
        ParticipantSnapshot {
            id: ResourceParticipantId::from_raw(id),
            activity,
            profile: ResourceProfile::new(
                ResourceAmount {
                    cpu_memory_bytes: 32 * MIB,
                    gpu_memory_bytes: 16 * MIB,
                    worker_slots: 0,
                    network_slots: 0,
                },
                ResourceAmount {
                    cpu_memory_bytes: 512 * MIB,
                    gpu_memory_bytes: 256 * MIB,
                    worker_slots: 4,
                    network_slots: 2,
                },
            ),
        }
    }

    #[test]
    fn auto_uses_saver_for_low_power_and_performance_for_large_systems() {
        let low_power = ResourceCoordinator::new(ResourceMode::Auto, system(32, 12, true));
        assert_eq!(low_power.plan(&[]).effective_mode, ResourceMode::Saver);

        let large = ResourceCoordinator::new(ResourceMode::Auto, system(32, 12, false));
        assert_eq!(large.plan(&[]).effective_mode, ResourceMode::Performance);
    }

    #[test]
    fn explicit_modes_are_never_overridden() {
        let coordinator = ResourceCoordinator::new(ResourceMode::Performance, system(4, 2, true));
        assert_eq!(
            coordinator.plan(&[]).effective_mode,
            ResourceMode::Performance
        );
    }

    #[test]
    fn allocations_never_exceed_budget_or_participant_targets() {
        let coordinator = ResourceCoordinator::new(ResourceMode::Saver, system(4, 4, false));
        let participants = vec![
            participant(1, ActivityLevel::ForegroundInteractive),
            participant(2, ActivityLevel::BackgroundWarm),
        ];
        let plan = coordinator.plan(&participants);
        assert!(
            plan.allocations
                .iter()
                .map(|allocation| allocation.amount.cpu_memory_bytes)
                .sum::<u64>()
                <= plan.budget.cpu_memory_bytes
        );
        assert!(
            plan.allocations
                .iter()
                .all(|allocation| allocation.amount.cpu_memory_bytes <= 512 * MIB)
        );
    }

    #[test]
    fn interactive_participants_win_constrained_capacity() {
        let coordinator = ResourceCoordinator::new(ResourceMode::Saver, system(2, 2, false));
        let participants = vec![
            participant(1, ActivityLevel::BackgroundCold),
            participant(2, ActivityLevel::ForegroundInteractive),
        ];
        let plan = coordinator.plan(&participants);
        assert!(
            plan.allocations[1].amount.cpu_memory_bytes
                > plan.allocations[0].amount.cpu_memory_bytes
        );
    }

    #[test]
    fn suspendable_participant_releases_all_resources() {
        let coordinator = ResourceCoordinator::new(ResourceMode::Balanced, system(16, 8, false));
        let plan = coordinator.plan(&[participant(1, ActivityLevel::Suspended)]);
        assert_eq!(plan.allocations[0].amount, ResourceAmount::default());
    }

    #[test]
    fn non_suspendable_participant_keeps_minimum_when_suspended() {
        let coordinator = ResourceCoordinator::new(ResourceMode::Balanced, system(16, 8, false));
        let mut snapshot = participant(1, ActivityLevel::Suspended);
        snapshot.profile.suspendable = false;
        let plan = coordinator.plan(&[snapshot]);
        assert_eq!(plan.allocations[0].amount.cpu_memory_bytes, 32 * MIB);
    }

    #[test]
    fn allocation_is_deterministic() {
        let coordinator = ResourceCoordinator::new(ResourceMode::Balanced, system(16, 8, false));
        let participants = vec![
            participant(1, ActivityLevel::ForegroundIdle),
            participant(2, ActivityLevel::ForegroundIdle),
        ];
        assert_eq!(
            coordinator.plan(&participants),
            coordinator.plan(&participants)
        );
    }
}
