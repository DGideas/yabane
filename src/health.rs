//! Runtime-only health for the identity layer.
//!
//! A credential that received an explicit rate-limit answer stays out of
//! selection until its cooldown elapses. Nothing here is persisted: how long an
//! upstream account stays exhausted is knowledge this instance does not own, so
//! a restart starts with every credential eligible again.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Identifies one credential inside its Endpoint. Credential IDs are unique
/// within an Endpoint only, so the Provider and Endpoint are part of the key.
pub fn credential_key(provider_id: &str, endpoint_id: &str, credential_id: &str) -> String {
    format!("{provider_id}/{endpoint_id}/{credential_id}")
}

#[derive(Clone, Default)]
pub struct CredentialHealth {
    cooldowns: Arc<Mutex<HashMap<String, Instant>>>,
}

impl CredentialHealth {
    pub fn is_cooling(&self, key: &str) -> bool {
        self.remaining(key).is_some()
    }

    pub fn remaining(&self, key: &str) -> Option<Duration> {
        let cooldowns = self.cooldowns.lock().expect("credential health lock");
        let until = cooldowns.get(key)?;
        let now = Instant::now();
        (*until > now).then(|| *until - now)
    }

    /// Whole seconds a credential stays out of selection, rounded up so a
    /// positive cooldown never reads as already over.
    pub fn remaining_seconds(&self, key: &str) -> Option<u64> {
        self.remaining(key).map(|remaining| {
            let seconds = remaining.as_secs();
            if remaining.subsec_nanos() > 0 {
                seconds + 1
            } else {
                seconds
            }
        })
    }

    pub fn cool_down(&self, key: String, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        let mut cooldowns = self.cooldowns.lock().expect("credential health lock");
        let now = Instant::now();
        cooldowns.retain(|_, until| *until > now);
        cooldowns.insert(key, now + duration);
    }

    pub fn clear(&self, key: &str) {
        self.cooldowns
            .lock()
            .expect("credential health lock")
            .remove(key);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{CredentialHealth, credential_key};

    #[test]
    fn cooldown_expires_and_can_be_cleared() {
        let health = CredentialHealth::default();
        let key = credential_key("openai", "zen", "account-a");

        assert!(!health.is_cooling(&key));
        health.cool_down(key.clone(), Duration::from_secs(60));
        assert!(health.is_cooling(&key));
        assert!(
            health
                .remaining_seconds(&key)
                .is_some_and(|value| value > 0)
        );
        health.clear(&key);
        assert!(!health.is_cooling(&key));
    }

    #[test]
    fn zero_duration_never_marks_a_credential() {
        let health = CredentialHealth::default();
        let key = credential_key("openai", "zen", "account-a");
        health.cool_down(key.clone(), Duration::ZERO);
        assert!(!health.is_cooling(&key));
    }

    #[test]
    fn keys_are_scoped_to_the_owning_endpoint() {
        let first = credential_key("openai", "zen", "account");
        let second = credential_key("openai", "other", "account");
        assert_ne!(first, second);
    }
}
