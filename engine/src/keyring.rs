//! Store key custody: the SQLCipher key lives only as a Secret Service item
//! in the user's default (login) collection.
//!
//! Never prompts: no `Unlock`, no `CreateCollection`, and never
//! `default_collection()` (it falls back to creating one, which makes
//! gnome-keyring show a new-keyring password dialog). Items are created or
//! deleted only right after `locked()` said `false` in the same attempt; a
//! user locking the keyring in the milliseconds in between may still get
//! gnome-keyring's own dialog, which is accepted.
//!
//! Error strings are fixed descriptions, never secrets or library messages.

use corelib::key::StoreKey;
use std::collections::HashMap;
use zeroize::Zeroizing;

const SCHEMA: &str = "org.nadelacruz.Correctiond.StoreKey";
const ITEM_LABEL: &str = "Correctiond learning store key";

/// The Secret Service operations we use, so tests can run against a fake.
#[async_trait::async_trait]
pub trait SecretBackend: Send + Sync {
    async fn locked(&self) -> Result<bool, String>;
    async fn find(
        &self,
        attrs: &HashMap<&'static str, String>,
    ) -> Result<Vec<Zeroizing<Vec<u8>>>, String>;
    async fn create(
        &self,
        attrs: &HashMap<&'static str, String>,
        secret: &[u8],
    ) -> Result<(), String>;
    async fn delete(&self, attrs: &HashMap<&'static str, String>) -> Result<(), String>;
}

pub fn key_attributes(purpose: &str) -> HashMap<&'static str, String> {
    HashMap::from([
        ("xdg:schema", SCHEMA.to_owned()),
        ("purpose", purpose.to_owned()),
        ("version", "1".to_owned()),
    ])
}

pub enum KeyState {
    /// Every parsable key item found (normally one).
    Ready(Vec<StoreKey>),
    Locked,
    /// Fixed reason text; never secret material.
    Unavailable(String),
}

/// The key for `purpose`, created on first use. Locked ⇒ `Locked` with no
/// other call. An existing item is never overwritten or added to, even when
/// it cannot be parsed.
pub async fn fetch_or_create_key_with<B: SecretBackend>(b: &B, purpose: &str) -> KeyState {
    match b.locked().await {
        Ok(true) => return KeyState::Locked,
        Ok(false) => {}
        Err(e) => return KeyState::Unavailable(e),
    }
    let attrs = key_attributes(purpose);
    let found = match b.find(&attrs).await {
        Ok(found) => found,
        Err(e) => return KeyState::Unavailable(e),
    };
    if !found.is_empty() {
        let keys: Vec<StoreKey> = found
            .iter()
            .filter_map(|s| StoreKey::from_hex(std::str::from_utf8(s).ok()?.trim()))
            .collect();
        if keys.is_empty() {
            return KeyState::Unavailable("malformed key item".into());
        }
        return KeyState::Ready(keys);
    }
    // A locked collection searches as empty: check again before creating.
    match b.locked().await {
        Ok(true) => return KeyState::Locked,
        Ok(false) => {}
        Err(e) => return KeyState::Unavailable(e),
    }
    let key = StoreKey::generate();
    let hex = key.to_hex();
    if let Err(e) = b.create(&attrs, hex.as_bytes()).await {
        return KeyState::Unavailable(e);
    }
    KeyState::Ready(vec![key])
}

/// Delete every item for `purpose`. Locked ⇒ `Err("locked")`, no call issued.
pub async fn delete_key_with<B: SecretBackend>(b: &B, purpose: &str) -> Result<(), String> {
    if b.locked().await? {
        return Err("locked".into());
    }
    b.delete(&key_attributes(purpose)).await
}

/// The real Secret Service over the session bus (oo7, D-Bus backend).
pub struct Oo7Backend {
    /// One session reused across calls; dropped after any failure so the
    /// next attempt reconnects (gnome-keyring may have restarted).
    service: tokio::sync::Mutex<Option<oo7::dbus::Service>>,
}

impl Default for Oo7Backend {
    fn default() -> Oo7Backend {
        Oo7Backend::new()
    }
}

impl Oo7Backend {
    pub fn new() -> Oo7Backend {
        Oo7Backend {
            service: tokio::sync::Mutex::new(None),
        }
    }

    /// The default collection, if it exists. Never creates one.
    async fn collection(&self) -> Result<oo7::dbus::Collection, String> {
        let mut cached = self.service.lock().await;
        if cached.is_none() {
            let service = oo7::dbus::Service::new()
                .await
                .map_err(|_| "secret service unavailable".to_owned())?;
            *cached = Some(service);
        }
        let Some(service) = cached.as_ref() else {
            return Err("secret service unavailable".into());
        };
        match service.with_alias("default").await {
            Ok(Some(c)) => Ok(c),
            Ok(None) => Err("no default collection".into()),
            Err(_) => {
                *cached = None;
                Err("secret service unavailable".into())
            }
        }
    }

    async fn reset(&self) {
        *self.service.lock().await = None;
    }

    async fn fail<T>(&self, what: &str) -> Result<T, String> {
        self.reset().await;
        Err(what.to_owned())
    }
}

#[async_trait::async_trait]
impl SecretBackend for Oo7Backend {
    async fn locked(&self) -> Result<bool, String> {
        let c = self.collection().await?;
        match c.is_locked().await {
            Ok(l) => Ok(l),
            Err(_) => self.fail("keyring state unavailable").await,
        }
    }

    async fn find(
        &self,
        attrs: &HashMap<&'static str, String>,
    ) -> Result<Vec<Zeroizing<Vec<u8>>>, String> {
        let c = self.collection().await?;
        let Ok(items) = c.search_items(attrs).await else {
            return self.fail("keyring search failed").await;
        };
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            match item.secret().await {
                // `Secret` zeroizes itself on drop.
                Ok(s) => out.push(Zeroizing::new(s.as_bytes().to_vec())),
                Err(_) => return self.fail("keyring read failed").await,
            }
        }
        Ok(out)
    }

    async fn create(
        &self,
        attrs: &HashMap<&'static str, String>,
        secret: &[u8],
    ) -> Result<(), String> {
        let c = self.collection().await?;
        // Stored as text so `secret-tool lookup` shows the hex as is.
        let secret = oo7::Secret::with_content_type(oo7::ContentType::Text, secret);
        match c.create_item(ITEM_LABEL, attrs, secret, false, None).await {
            Ok(_) => Ok(()),
            Err(_) => self.fail("keyring store failed").await,
        }
    }

    async fn delete(&self, attrs: &HashMap<&'static str, String>) -> Result<(), String> {
        let c = self.collection().await?;
        let Ok(items) = c.search_items(attrs).await else {
            return self.fail("keyring search failed").await;
        };
        for item in items {
            if item.delete(None).await.is_err() {
                return self.fail("keyring delete failed").await;
            }
        }
        Ok(())
    }
}

/// In-memory Secret Service stand-in that records every call.
#[cfg(test)]
pub mod fake {
    use super::SecretBackend;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use zeroize::Zeroizing;

    type Attrs = HashMap<&'static str, String>;

    #[derive(Default)]
    pub struct FakeState {
        /// `locked()` answer once `locked_calls` is used up.
        pub locked: bool,
        /// The next this-many `locked()` calls return true.
        pub locked_calls: u32,
        pub items: Vec<(Attrs, Vec<u8>)>,
        pub log: Vec<&'static str>,
        /// `find` takes this long (virtual time), to catch an attempt mid-way.
        pub find_delay: Option<Duration>,
        /// `delete` fails even when unlocked.
        pub fail_delete: bool,
        /// `delete` never returns (the process dies during the call).
        pub delete_hang: bool,
    }

    #[derive(Clone, Default)]
    pub struct FakeBackend(pub Arc<Mutex<FakeState>>);

    impl FakeBackend {
        pub fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.0.lock().unwrap_or_else(|e| e.into_inner())
        }
        pub fn log(&self) -> Vec<&'static str> {
            self.state().log.clone()
        }
        pub fn count(&self, call: &str) -> usize {
            self.state().log.iter().filter(|c| **c == call).count()
        }
        pub fn set_locked(&self, on: bool) {
            self.state().locked = on;
        }
        pub fn clear_log(&self) {
            self.state().log.clear();
        }
        /// Secrets of every stored item, in creation order.
        pub fn secrets(&self) -> Vec<Vec<u8>> {
            self.state().items.iter().map(|(_, s)| s.clone()).collect()
        }
        pub fn insert(&self, attrs: Attrs, secret: &[u8]) {
            self.state().items.push((attrs, secret.to_vec()));
        }
    }

    fn matches(item: &Attrs, query: &Attrs) -> bool {
        query.iter().all(|(k, v)| item.get(k) == Some(v))
    }

    #[async_trait::async_trait]
    impl SecretBackend for FakeBackend {
        async fn locked(&self) -> Result<bool, String> {
            let mut s = self.state();
            s.log.push("locked");
            if s.locked_calls > 0 {
                s.locked_calls -= 1;
                return Ok(true);
            }
            Ok(s.locked)
        }

        async fn find(&self, attrs: &Attrs) -> Result<Vec<Zeroizing<Vec<u8>>>, String> {
            let delay = {
                let mut s = self.state();
                s.log.push("find");
                s.find_delay
            };
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
            }
            let s = self.state();
            if s.locked {
                // A locked collection searches as empty.
                return Ok(Vec::new());
            }
            Ok(s.items
                .iter()
                .filter(|(a, _)| matches(a, attrs))
                .map(|(_, v)| Zeroizing::new(v.clone()))
                .collect())
        }

        async fn create(&self, attrs: &Attrs, secret: &[u8]) -> Result<(), String> {
            let mut s = self.state();
            s.log.push("create");
            if s.locked {
                return Err("locked".into());
            }
            s.items.push((attrs.clone(), secret.to_vec()));
            Ok(())
        }

        async fn delete(&self, attrs: &Attrs) -> Result<(), String> {
            let hang = {
                let mut s = self.state();
                s.log.push("delete");
                s.delete_hang
            };
            if hang {
                std::future::pending::<()>().await;
            }
            let mut s = self.state();
            if s.locked || s.fail_delete {
                return Err("locked".into());
            }
            s.items.retain(|(a, _)| !matches(a, attrs));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeBackend;
    use super::*;

    #[test]
    fn attributes_are_stable() {
        let a = key_attributes("learn-db-key");
        assert_eq!(a.len(), 3);
        assert_eq!(a["xdg:schema"], "org.nadelacruz.Correctiond.StoreKey");
        assert_eq!(a["purpose"], "learn-db-key");
        assert_eq!(a["version"], "1");
    }

    #[tokio::test(start_paused = true)]
    async fn locked_issues_no_other_calls() {
        let b = FakeBackend::default();
        b.set_locked(true);
        assert!(matches!(
            fetch_or_create_key_with(&b, "learn-db-key").await,
            KeyState::Locked
        ));
        assert_eq!(b.log(), ["locked"]);
        assert_eq!(
            delete_key_with(&b, "learn-db-key").await,
            Err("locked".into())
        );
        assert_eq!(b.log(), ["locked", "locked"]);
    }

    #[tokio::test(start_paused = true)]
    async fn never_creates_when_item_exists() {
        let b = FakeBackend::default();
        b.insert(key_attributes("learn-db-key"), b"not hex at all");
        match fetch_or_create_key_with(&b, "learn-db-key").await {
            KeyState::Unavailable(r) => assert_eq!(r, "malformed key item"),
            _ => panic!("expected Unavailable"),
        }
        assert_eq!(b.log(), ["locked", "find"]);
        assert_eq!(b.secrets(), [b"not hex at all".to_vec()]);
    }

    #[tokio::test(start_paused = true)]
    async fn creates_once_then_reuses() {
        let b = FakeBackend::default();
        let KeyState::Ready(first) = fetch_or_create_key_with(&b, "learn-db-key").await else {
            panic!("expected Ready");
        };
        assert_eq!(b.log(), ["locked", "find", "locked", "create"]);
        let KeyState::Ready(again) = fetch_or_create_key_with(&b, "learn-db-key").await else {
            panic!("expected Ready");
        };
        assert_eq!(b.count("create"), 1);
        assert_eq!(first.len(), 1);
        assert_eq!(*first[0].to_hex(), *again[0].to_hex());
        // A different purpose is a different item.
        let KeyState::Ready(other) = fetch_or_create_key_with(&b, "test-key").await else {
            panic!("expected Ready");
        };
        assert_ne!(*other[0].to_hex(), *first[0].to_hex());
    }

    #[tokio::test(start_paused = true)]
    async fn malformed_items_are_skipped_not_replaced() {
        let b = FakeBackend::default();
        let good = corelib::key::StoreKey::generate();
        b.insert(key_attributes("learn-db-key"), b"junk");
        b.insert(key_attributes("learn-db-key"), good.to_hex().as_bytes());
        let KeyState::Ready(keys) = fetch_or_create_key_with(&b, "learn-db-key").await else {
            panic!("expected Ready");
        };
        assert_eq!(keys.len(), 1);
        assert_eq!(*keys[0].to_hex(), *good.to_hex());
        assert_eq!(b.count("create"), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn locked_between_find_and_create_does_not_create() {
        let b = FakeBackend::default();
        // Unlocked at the first check, locked by the user during `find`.
        b.state().find_delay = Some(std::time::Duration::from_millis(50));
        let b2 = b.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            b2.set_locked(true);
        });
        assert!(matches!(
            fetch_or_create_key_with(&b, "learn-db-key").await,
            KeyState::Locked
        ));
        assert_eq!(b.count("create"), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn delete_removes_item_when_unlocked() {
        let b = FakeBackend::default();
        let _ = fetch_or_create_key_with(&b, "learn-db-key").await;
        assert_eq!(b.secrets().len(), 1);
        b.clear_log();
        assert_eq!(delete_key_with(&b, "learn-db-key").await, Ok(()));
        assert_eq!(b.log(), ["locked", "delete"]);
        assert!(b.secrets().is_empty());
    }

    /// Touches the real login keyring: run only by hand on the desktop,
    /// `cargo test -p engine live_keyring -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_keyring_roundtrip() {
        let b = Oo7Backend::new();
        let KeyState::Ready(a) = fetch_or_create_key_with(&b, "test-key").await else {
            panic!("keyring not ready (locked or unavailable)");
        };
        let KeyState::Ready(again) = fetch_or_create_key_with(&b, "test-key").await else {
            panic!("keyring not ready on the second fetch");
        };
        assert_eq!(*a[0].to_hex(), *again[0].to_hex());
        assert_eq!(delete_key_with(&b, "test-key").await, Ok(()));
    }
}
