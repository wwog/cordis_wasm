use crate::{CordisError, Disposer, EffectScope};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static NEXT_TRACKING_ID: AtomicU64 = AtomicU64::new(1);

/// Stable identity for one effect-owned collection registration.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrackingId(u64);

impl TrackingId {
    fn next() -> Self {
        let id = NEXT_TRACKING_ID.fetch_add(1, Ordering::Relaxed);
        assert_ne!(id, u64::MAX, "TrackingId space exhausted");
        Self(id)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Effect-aware ordered registrations that remove by identity, not value equality.
#[derive(Clone, Debug, Default)]
pub struct TrackedList<T> {
    values: Arc<Mutex<BTreeMap<TrackingId, T>>>,
}

impl<T: Clone + Send + 'static> TrackedList<T> {
    /// Inserts a value and registers its exact inverse in `scope`.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] when the scope is disposing.
    pub fn insert(&self, value: T, scope: &EffectScope) -> Result<TrackingId, CordisError> {
        let id = TrackingId::next();
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, value);
        let values = self.values.clone();
        if let Err(error) = scope.defer(Disposer::infallible(move || async move {
            values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&id);
        })) {
            self.values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&id);
            return Err(error);
        }
        Ok(id)
    }

    pub fn snapshot(&self) -> Vec<(TrackingId, T)> {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(id, value)| (*id, value.clone()))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Effect-aware keyed registrations that allow equal keys without cleanup collisions.
#[derive(Clone, Debug, Default)]
pub struct TrackedMap<K, V> {
    values: Arc<Mutex<BTreeMap<K, BTreeMap<TrackingId, V>>>>,
}

impl<K, V> TrackedMap<K, V>
where
    K: Clone + Ord + Send + 'static,
    V: Clone + Send + 'static,
{
    /// Inserts one keyed registration and registers its exact inverse in `scope`.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] when the scope is disposing.
    pub fn insert(
        &self,
        key: &K,
        value: V,
        scope: &EffectScope,
    ) -> Result<TrackingId, CordisError> {
        let id = TrackingId::next();
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key.clone())
            .or_default()
            .insert(id, value);
        let values = self.values.clone();
        let cleanup_key = key.clone();
        if let Err(error) = scope.defer(Disposer::infallible(move || async move {
            let mut values = values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(registrations) = values.get_mut(&cleanup_key) {
                registrations.remove(&id);
                if registrations.is_empty() {
                    values.remove(&cleanup_key);
                }
            }
        })) {
            let mut values = self
                .values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(registrations) = values.get_mut(key) {
                registrations.remove(&id);
                if registrations.is_empty() {
                    values.remove(key);
                }
            }
            return Err(error);
        }
        Ok(id)
    }

    pub fn snapshot(&self, key: &K) -> Vec<(TrackingId, V)> {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .into_iter()
            .flat_map(|values| values.iter())
            .map(|(id, value)| (*id, value.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EffectGuard;

    #[tokio::test]
    async fn equal_values_are_removed_by_registration_identity() {
        let (first, first_scope) = EffectGuard::new("first");
        let (second, second_scope) = EffectGuard::new("second");
        let list = TrackedList::default();
        let first_id = list.insert("same", &first_scope).unwrap();
        let second_id = list.insert("same", &second_scope).unwrap();
        assert_ne!(first_id, second_id);
        first.dispose().await.unwrap();
        assert_eq!(list.snapshot(), vec![(second_id, "same")]);
        second.dispose().await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn equal_map_keys_keep_independent_registrations() {
        let (first, first_scope) = EffectGuard::new("first");
        let (_second, second_scope) = EffectGuard::new("second");
        let map = TrackedMap::default();
        map.insert(&"key", 1, &first_scope).unwrap();
        let remaining = map.insert(&"key", 2, &second_scope).unwrap();
        first.dispose().await.unwrap();
        assert_eq!(map.snapshot(&"key"), vec![(remaining, 2)]);
    }
}
