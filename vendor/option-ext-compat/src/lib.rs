//! Independently written compatibility methods used by `dirs-sys`.

/// Additional operations for `Option` with the callback argument first.
pub trait OptionExt<T> {
    fn contains<U>(&self, expected: &U) -> bool
    where
        U: PartialEq<T>;

    fn map_or2<U, F>(self, transform: F, default: U) -> U
    where
        F: FnOnce(T) -> U;

    fn map_or_else2<U, F, D>(self, transform: F, default: D) -> U
    where
        F: FnOnce(T) -> U,
        D: FnOnce() -> U;
}

impl<T> OptionExt<T> for Option<T> {
    fn contains<U>(&self, expected: &U) -> bool
    where
        U: PartialEq<T>,
    {
        match self {
            Some(value) => expected == value,
            None => false,
        }
    }

    fn map_or2<U, F>(self, transform: F, default: U) -> U
    where
        F: FnOnce(T) -> U,
    {
        match self {
            Some(value) => transform(value),
            None => default,
        }
    }

    fn map_or_else2<U, F, D>(self, transform: F, default: D) -> U
    where
        F: FnOnce(T) -> U,
        D: FnOnce() -> U,
    {
        match self {
            Some(value) => transform(value),
            None => default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OptionExt;

    #[test]
    fn matches_option_behavior() {
        assert!(OptionExt::contains(&Some(7), &7));
        assert_eq!(Some(7).map_or2(|value| value * 2, 3), 14);
        assert_eq!(None::<i32>.map_or_else2(|value| value * 2, || 3), 3);
    }
}
