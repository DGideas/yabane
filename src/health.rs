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

use serde::Serialize;

/// Identifies one credential inside its Endpoint. Credential IDs are unique
/// within an Endpoint only, so the Provider and Endpoint are part of the key.
pub fn credential_key(provider_id: &str, endpoint_id: &str, credential_id: &str) -> String {
    format!("{provider_id}/{endpoint_id}/{credential_id}")
}

/// Identifies the Endpoint whose cooldown policy is being observed.
pub fn endpoint_key(provider_id: &str, endpoint_id: &str) -> String {
    format!("{provider_id}/{endpoint_id}")
}

/// Why a delay was selected at the time of the answer, not under today's policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CooldownSource {
    Fixed,
    Provider,
    Capped,
    Fallback,
}

/// What an Endpoint's cooldown policy has done since this instance started.
///
/// A policy whose length comes from the Provider can be configured correctly and
/// still never arm, so the console reports what was observed instead of leaving
/// an administrator unable to tell a working policy from a dead one.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CooldownActivity {
    /// Rate-limit answers that selected a duration, including zero (no new cooldown).
    pub applied: u64,
    /// Rate-limit answers the policy deliberately did nothing about, which is
    /// possible only when the Provider reported no usable delay.
    pub skipped: u64,
    /// Length of the most recent cooldown this policy armed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seconds: Option<u64>,
    /// The delay the Provider's own answer asked for when that cooldown was
    /// armed, when the answer carried an integer number of seconds. `None`
    /// means it carried no delay this instance reads, so a length the Provider
    /// suggested, one the configured ceiling held down, and the configured
    /// fallback stay distinguishable from each other.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_reported_seconds: Option<u64>,
    /// The reason the most recent delay was selected. Policy edits do not rewrite it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_source: Option<CooldownSource>,
    /// When that cooldown was armed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_applied_at: Option<u64>,
    /// When this Endpoint last answered a request with a rate limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_observed_at: Option<u64>,
}

#[derive(Clone, Default)]
pub struct CredentialHealth {
    cooldowns: Arc<Mutex<HashMap<String, Instant>>>,
    activity: Arc<Mutex<HashMap<String, CooldownActivity>>>,
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

    /// What this Endpoint's cooldown policy has done so far. An Endpoint that
    /// has not seen a rate-limit answer reports nothing but zeroes.
    pub fn activity(&self, key: &str) -> CooldownActivity {
        self.activity
            .lock()
            .expect("credential health lock")
            .get(key)
            .copied()
            .unwrap_or_default()
    }

    pub fn record_cooldown_applied(
        &self,
        key: String,
        seconds: u64,
        reported: Option<u64>,
        source: CooldownSource,
        at: u64,
    ) {
        let mut activity = self.activity.lock().expect("credential health lock");
        let entry = activity.entry(key).or_default();
        entry.applied += 1;
        entry.last_seconds = Some(seconds);
        entry.last_reported_seconds = reported;
        entry.last_source = Some(source);
        entry.last_applied_at = Some(at);
        entry.last_observed_at = Some(at);
    }

    pub fn record_cooldown_skipped(&self, key: String, at: u64) {
        let mut activity = self.activity.lock().expect("credential health lock");
        let entry = activity.entry(key).or_default();
        entry.skipped += 1;
        entry.last_observed_at = Some(at);
    }

    /// Forgets every cooldown and observation recorded for one Endpoint, so a
    /// renamed or deleted Endpoint cannot inherit another one's history.
    pub fn forget_endpoint(&self, key: &str) {
        let prefix = format!("{key}/");
        self.cooldowns
            .lock()
            .expect("credential health lock")
            .retain(|stored, _| !stored.starts_with(&prefix));
        self.activity
            .lock()
            .expect("credential health lock")
            .remove(key);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{CooldownActivity, CooldownSource, CredentialHealth, credential_key, endpoint_key};

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

    #[test]
    fn activity_counts_what_the_policy_did() {
        let health = CredentialHealth::default();
        let key = endpoint_key("openai", "zen");

        assert_eq!(health.activity(&key), CooldownActivity::default());
        health.record_cooldown_skipped(key.clone(), 1_700_000_000);
        health.record_cooldown_applied(
            key.clone(),
            120,
            Some(300),
            CooldownSource::Capped,
            1_700_000_060,
        );
        let activity = health.activity(&key);
        assert_eq!(activity.applied, 1);
        assert_eq!(activity.skipped, 1);
        assert_eq!(activity.last_seconds, Some(120));
        assert_eq!(activity.last_reported_seconds, Some(300));
        assert_eq!(activity.last_source, Some(CooldownSource::Capped));
        assert_eq!(activity.last_applied_at, Some(1_700_000_060));
        assert_eq!(activity.last_observed_at, Some(1_700_000_060));
        // A later arming with no reported delay replaces the number instead of
        // leaving a stale one next to the length that was actually used.
        health.record_cooldown_applied(
            key.clone(),
            60,
            None,
            CooldownSource::Fallback,
            1_700_000_120,
        );
        assert_eq!(health.activity(&key).last_reported_seconds, None);
        assert_eq!(
            health.activity(&key).last_source,
            Some(CooldownSource::Fallback)
        );
        // Zero is a reported delay, not a missing value. Serialization keeps it.
        health.record_cooldown_applied(
            key.clone(),
            0,
            Some(0),
            CooldownSource::Provider,
            1_700_000_180,
        );
        let before_skip = health.activity(&key);
        let serialized = serde_json::to_value(before_skip).unwrap();
        assert_eq!(serialized["last_reported_seconds"], 0);
        assert_eq!(serialized["last_source"], "provider");
        health.record_cooldown_skipped(key.clone(), 1_700_000_240);
        assert_eq!(health.activity(&key).last_source, before_skip.last_source);
        assert_eq!(health.activity(&key).last_seconds, before_skip.last_seconds);
        // A skipped answer is observed but arms nothing.
        let skipped = CredentialHealth::default();
        skipped.record_cooldown_skipped(endpoint_key("openai", "zen"), 1_700_000_000);
        assert_eq!(
            skipped
                .activity(&endpoint_key("openai", "zen"))
                .last_applied_at,
            None
        );
    }

    #[test]
    fn forgetting_an_endpoint_clears_its_credentials_and_history() {
        let health = CredentialHealth::default();
        let key = credential_key("openai", "zen", "account");
        health.cool_down(key.clone(), Duration::from_secs(60));
        health.record_cooldown_applied(
            endpoint_key("openai", "zen"),
            60,
            None,
            CooldownSource::Fixed,
            1,
        );

        health.forget_endpoint(&endpoint_key("openai", "zen"));
        assert!(!health.is_cooling(&key));
        assert_eq!(
            health.activity(&endpoint_key("openai", "zen")),
            CooldownActivity::default()
        );
        // Another Endpoint of the same Provider is untouched.
        let other = credential_key("openai", "other", "account");
        health.cool_down(other.clone(), Duration::from_secs(60));
        health.forget_endpoint(&endpoint_key("openai", "zen"));
        assert!(health.is_cooling(&other));
    }
}
