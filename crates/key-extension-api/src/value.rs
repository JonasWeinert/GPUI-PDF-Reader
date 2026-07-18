use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A bounded, runtime-neutral value used for command payloads, snapshots, and
/// capability calls. Hosts validate its size before retaining or dispatching it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum DataValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Number(f64),
    String(String),
    List(Vec<Self>),
    Record(BTreeMap<String, Self>),
}

impl DataValue {
    #[must_use]
    pub fn kind(&self) -> DataKind {
        match self {
            Self::Null => DataKind::Null,
            Self::Boolean(_) => DataKind::Boolean,
            Self::Integer(_) => DataKind::Integer,
            Self::Number(_) => DataKind::Number,
            Self::String(_) => DataKind::String,
            Self::List(_) => DataKind::List,
            Self::Record(_) => DataKind::Record,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataKind {
    Null,
    Boolean,
    Integer,
    Number,
    String,
    List,
    Record,
}

/// A lookup into a host-provided immutable state snapshot. Segments are keys,
/// never executable expressions.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StateBinding(pub Vec<String>);

impl StateBinding {
    #[must_use]
    pub fn new(segments: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self(segments.into_iter().map(Into::into).collect())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", content = "value", rename_all = "snake_case")]
pub enum BooleanSource {
    Constant(bool),
    Binding(StateBinding),
}

impl Default for BooleanSource {
    fn default() -> Self {
        Self::Constant(true)
    }
}
