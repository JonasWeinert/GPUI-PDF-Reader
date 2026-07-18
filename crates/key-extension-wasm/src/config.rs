use std::time::Duration;

use crate::{WasmDiagnostic, WasmDiagnosticCode, WasmStage};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmRuntimeLimits {
    pub maximum_component_bytes: usize,
    pub maximum_memory_bytes: usize,
    pub maximum_table_elements: usize,
    pub maximum_instances: usize,
    pub maximum_memories: usize,
    pub maximum_tables: usize,
    pub maximum_host_resources: usize,
    pub maximum_host_calls_per_invocation: u64,
    pub maximum_input_bytes: usize,
    pub maximum_output_bytes: usize,
    pub maximum_effects_per_event: usize,
    pub fuel_per_invocation: u64,
    pub epoch_ticks_per_invocation: u64,
    pub epoch_tick_interval: Duration,
    pub maximum_wasm_stack_bytes: usize,
}

impl Default for WasmRuntimeLimits {
    fn default() -> Self {
        Self {
            maximum_component_bytes: 16 * 1024 * 1024,
            maximum_memory_bytes: 64 * 1024 * 1024,
            maximum_table_elements: 10_000,
            maximum_instances: 16,
            maximum_memories: 4,
            maximum_tables: 4,
            maximum_host_resources: 1_024,
            maximum_host_calls_per_invocation: 256,
            maximum_input_bytes: 1024 * 1024,
            maximum_output_bytes: 4 * 1024 * 1024,
            maximum_effects_per_event: 64,
            fuel_per_invocation: 10_000_000,
            epoch_ticks_per_invocation: 50,
            epoch_tick_interval: Duration::from_millis(10),
            maximum_wasm_stack_bytes: 512 * 1024,
        }
    }
}

impl WasmRuntimeLimits {
    pub fn validate(&self) -> Result<(), WasmDiagnostic> {
        let positive = [
            ("maximum_component_bytes", self.maximum_component_bytes),
            ("maximum_memory_bytes", self.maximum_memory_bytes),
            ("maximum_table_elements", self.maximum_table_elements),
            ("maximum_instances", self.maximum_instances),
            ("maximum_memories", self.maximum_memories),
            ("maximum_tables", self.maximum_tables),
            ("maximum_host_resources", self.maximum_host_resources),
            ("maximum_input_bytes", self.maximum_input_bytes),
            ("maximum_output_bytes", self.maximum_output_bytes),
            ("maximum_effects_per_event", self.maximum_effects_per_event),
            ("maximum_wasm_stack_bytes", self.maximum_wasm_stack_bytes),
        ];
        if let Some((name, _)) = positive.into_iter().find(|(_, value)| *value == 0) {
            return Err(configuration(format!("{name} must be greater than zero")));
        }
        if self.maximum_host_calls_per_invocation == 0 {
            return Err(configuration(
                "maximum_host_calls_per_invocation must be greater than zero",
            ));
        }
        if self.fuel_per_invocation == 0 {
            return Err(configuration(
                "fuel_per_invocation must be greater than zero",
            ));
        }
        if self.epoch_ticks_per_invocation == 0 || self.epoch_tick_interval.is_zero() {
            return Err(configuration(
                "epoch deadline and tick interval must be greater than zero",
            ));
        }
        if self.maximum_input_bytes > self.maximum_memory_bytes
            || self.maximum_output_bytes > self.maximum_memory_bytes
        {
            return Err(configuration(
                "input and output limits cannot exceed the per-memory limit",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn approximate_deadline(&self) -> Duration {
        self.epoch_tick_interval
            .saturating_mul(u32::try_from(self.epoch_ticks_per_invocation).unwrap_or(u32::MAX))
    }
}

fn configuration(message: impl Into<String>) -> WasmDiagnostic {
    WasmDiagnostic::new(
        WasmDiagnosticCode::InvalidConfiguration,
        WasmStage::Configuration,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_bounded_and_coherent() {
        let limits = WasmRuntimeLimits::default();
        limits.validate().unwrap();
        assert_eq!(limits.approximate_deadline(), Duration::from_millis(500));
    }

    #[test]
    fn rejects_output_larger_than_linear_memory() {
        let limits = WasmRuntimeLimits {
            maximum_output_bytes: 2,
            maximum_memory_bytes: 1,
            ..WasmRuntimeLimits::default()
        };
        assert_eq!(
            limits.validate().unwrap_err().code,
            WasmDiagnosticCode::InvalidConfiguration
        );
    }
}
