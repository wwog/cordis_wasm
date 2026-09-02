use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u64);

        impl $name {
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

define_id!(EffectId);
define_id!(FiberId);
define_id!(ListenerId);
define_id!(RealmId);

static NEXT_EFFECT_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_FIBER_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_LISTENER_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_REALM_ID: AtomicU64 = AtomicU64::new(1);

fn next_id(counter: &AtomicU64, kind: &str) -> u64 {
    let value = counter.fetch_add(1, Ordering::Relaxed);
    assert!(value != u64::MAX, "{kind} space exhausted");
    value
}

impl EffectId {
    pub(crate) fn next() -> Self {
        Self(next_id(&NEXT_EFFECT_ID, "EffectId"))
    }
}

impl FiberId {
    pub(crate) fn next() -> Self {
        Self(next_id(&NEXT_FIBER_ID, "FiberId"))
    }
}

impl ListenerId {
    pub(crate) fn next() -> Self {
        Self(next_id(&NEXT_LISTENER_ID, "ListenerId"))
    }
}

impl RealmId {
    pub(crate) fn next() -> Self {
        Self(next_id(&NEXT_REALM_ID, "RealmId"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_non_zero_and_monotonic() {
        let first = EffectId::next();
        let second = EffectId::next();
        assert!(first.get() > 0);
        assert!(second > first);
    }

    #[test]
    fn other_runtime_ids_are_non_zero_and_monotonic() {
        let first_fiber = FiberId::next();
        let second_fiber = FiberId::next();
        let first_realm = RealmId::next();
        let second_realm = RealmId::next();
        let first_listener = ListenerId::next();
        let second_listener = ListenerId::next();
        assert!(first_fiber.get() > 0);
        assert!(second_fiber > first_fiber);
        assert!(first_realm.get() > 0);
        assert!(second_realm > first_realm);
        assert!(first_listener.get() > 0);
        assert!(second_listener > first_listener);
    }
}
