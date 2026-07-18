use std::{collections::BTreeMap, error::Error, fmt};

use key_extension_api::{BooleanSource, DataValue, StateBinding};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateMapLimits {
    pub maximum_entries: usize,
    pub maximum_depth: usize,
    pub maximum_list_items: usize,
    pub maximum_string_bytes: usize,
}

impl Default for StateMapLimits {
    fn default() -> Self {
        Self {
            maximum_entries: 4_096,
            maximum_depth: 16,
            maximum_list_items: 1_024,
            maximum_string_bytes: 16_384,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateMapError {
    EntryLimit,
    DepthLimit,
    ListLimit,
    StringLimit,
    NonFiniteNumber,
    InvalidKey,
}

impl fmt::Display for StateMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EntryLimit => "state map contains too many values",
            Self::DepthLimit => "state map exceeds the nesting limit",
            Self::ListLimit => "state map list exceeds the item limit",
            Self::StringLimit => "state map string exceeds the byte limit",
            Self::NonFiniteNumber => "state map numbers must be finite",
            Self::InvalidKey => "state map contains an invalid key",
        })
    }
}

impl Error for StateMapError {}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoundedStateMap {
    values: BTreeMap<String, DataValue>,
    limits: StateMapLimits,
}

impl BoundedStateMap {
    pub fn new(
        values: BTreeMap<String, DataValue>,
        limits: StateMapLimits,
    ) -> Result<Self, StateMapError> {
        let mut entries = 0;
        validate_record(&values, &limits, 1, &mut entries)?;
        Ok(Self { values, limits })
    }

    #[must_use]
    pub fn limits(&self) -> &StateMapLimits {
        &self.limits
    }

    #[must_use]
    pub fn values(&self) -> &BTreeMap<String, DataValue> {
        &self.values
    }

    #[must_use]
    pub fn resolve(&self, binding: &StateBinding) -> Option<&DataValue> {
        let (first, rest) = binding.0.split_first()?;
        let mut value = self.values.get(first)?;
        for segment in rest {
            let DataValue::Record(record) = value else {
                return None;
            };
            value = record.get(segment)?;
        }
        Some(value)
    }

    #[must_use]
    pub fn resolve_boolean(&self, source: &BooleanSource) -> bool {
        match source {
            BooleanSource::Constant(value) => *value,
            BooleanSource::Binding(binding) => {
                matches!(self.resolve(binding), Some(DataValue::Boolean(true)))
            }
        }
    }

    #[must_use]
    pub fn resolve_string(&self, binding: &StateBinding) -> Option<&str> {
        match self.resolve(binding) {
            Some(DataValue::String(value)) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub fn selected_index(&self, binding: &StateBinding, ids: &[&str]) -> Option<usize> {
        match self.resolve(binding) {
            Some(DataValue::Integer(index)) => usize::try_from(*index)
                .ok()
                .filter(|index| *index < ids.len()),
            Some(DataValue::String(id)) => ids.iter().position(|candidate| *candidate == id),
            _ => None,
        }
    }
}

fn validate_record(
    record: &BTreeMap<String, DataValue>,
    limits: &StateMapLimits,
    depth: usize,
    entries: &mut usize,
) -> Result<(), StateMapError> {
    if depth > limits.maximum_depth {
        return Err(StateMapError::DepthLimit);
    }
    if record.len() > limits.maximum_list_items {
        return Err(StateMapError::ListLimit);
    }
    for (key, value) in record {
        if key.is_empty()
            || key.len() > limits.maximum_string_bytes
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(StateMapError::InvalidKey);
        }
        validate_value(value, limits, depth, entries)?;
    }
    Ok(())
}

fn validate_value(
    value: &DataValue,
    limits: &StateMapLimits,
    depth: usize,
    entries: &mut usize,
) -> Result<(), StateMapError> {
    *entries = entries.saturating_add(1);
    if *entries > limits.maximum_entries {
        return Err(StateMapError::EntryLimit);
    }
    match value {
        DataValue::Number(number) if !number.is_finite() => Err(StateMapError::NonFiniteNumber),
        DataValue::String(value) if value.len() > limits.maximum_string_bytes => {
            Err(StateMapError::StringLimit)
        }
        DataValue::List(values) => {
            if values.len() > limits.maximum_list_items {
                return Err(StateMapError::ListLimit);
            }
            if depth >= limits.maximum_depth && !values.is_empty() {
                return Err(StateMapError::DepthLimit);
            }
            for value in values {
                validate_value(value, limits, depth + 1, entries)?;
            }
            Ok(())
        }
        DataValue::Record(record) => validate_record(record, limits, depth + 1, entries),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(parts: &[&str]) -> StateBinding {
        StateBinding::new(parts.iter().copied())
    }

    #[test]
    fn resolves_nested_typed_values_without_expressions() {
        let values = BTreeMap::from([
            (
                "document".into(),
                DataValue::Record(BTreeMap::from([
                    ("title".into(), DataValue::String("Paper".into())),
                    ("ready".into(), DataValue::Boolean(true)),
                ])),
            ),
            ("active-tab".into(), DataValue::String("details".into())),
        ]);
        let state = BoundedStateMap::new(values, StateMapLimits::default()).unwrap();
        assert_eq!(
            state.resolve_string(&binding(&["document", "title"])),
            Some("Paper")
        );
        assert!(state.resolve_boolean(&BooleanSource::Binding(binding(&["document", "ready"]))));
        assert_eq!(
            state.selected_index(
                &binding(&["active-tab"]),
                &["overview", "details", "access"]
            ),
            Some(1)
        );
    }

    #[test]
    fn missing_or_wrong_typed_boolean_is_false() {
        let state = BoundedStateMap::default();
        assert!(!state.resolve_boolean(&BooleanSource::Binding(binding(&["missing"]))));
        assert!(state.resolve_boolean(&BooleanSource::Constant(true)));
    }

    #[test]
    fn rejects_unbounded_or_unsafe_state() {
        let limits = StateMapLimits {
            maximum_entries: 1,
            maximum_string_bytes: 4,
            ..StateMapLimits::default()
        };
        assert_eq!(
            BoundedStateMap::new(
                BTreeMap::from([("too-long".into(), DataValue::Null)]),
                limits.clone()
            )
            .unwrap_err(),
            StateMapError::InvalidKey
        );
        assert_eq!(
            BoundedStateMap::new(
                BTreeMap::from([("ok".into(), DataValue::String("large".into()))]),
                limits
            )
            .unwrap_err(),
            StateMapError::StringLimit
        );
    }
}
