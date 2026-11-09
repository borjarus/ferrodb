//! # ferrodb-secrets
//!
//! Secrets (passwords, tokens, key passphrases) never touch configuration
//! files: those hold a reference `secret://<id>` and the values live in the
//! system keyring — Windows Credential Manager, macOS Keychain, Secret
//! Service on Linux (plan, section 5.10; ADR-013).
//!
//! All secrets of one data source are stored as one credential (a small JSON
//! object), so a source is saved, read and removed atomically.
//!
//! Calls may block (D-Bus on Linux): run them off the UI thread.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
};

pub const SCHEME: &str = "secret://";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("invalid secret reference `{0}` (expected secret://<id>)")]
    BadReference(String),
    #[error("keyring: {0}")]
    Keyring(String),
    #[error("stored secret is corrupted: {0}")]
    Corrupted(String),
}

/// Key → value pairs, e.g. `password` → `…`.
pub type Secrets = BTreeMap<String, String>;

pub trait SecretStore: Send + Sync {
    /// Secrets under `reference`; empty when nothing is stored.
    fn get(&self, reference: &str) -> Result<Secrets, SecretError>;
    /// Replaces everything stored under `reference` (empty = delete).
    fn set(&self, reference: &str, secrets: &Secrets) -> Result<(), SecretError>;
    fn delete(&self, reference: &str) -> Result<(), SecretError> {
        self.set(reference, &Secrets::new())
    }
}

/// `secret://<id>` → `<id>`.
pub fn parse_reference(reference: &str) -> Result<&str, SecretError> {
    reference
        .strip_prefix(SCHEME)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| SecretError::BadReference(reference.to_owned()))
}

pub fn reference(id: &str) -> String {
    format!("{SCHEME}{id}")
}

/// The operating system keyring.
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    /// `service` namespaces the credentials (e.g. `FerroDb`).
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, reference: &str) -> Result<keyring::Entry, SecretError> {
        let id = parse_reference(reference)?;
        keyring::Entry::new(&self.service, id).map_err(|e| SecretError::Keyring(e.to_string()))
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, reference: &str) -> Result<Secrets, SecretError> {
        match self.entry(reference)?.get_password() {
            Ok(json) => {
                serde_json::from_str(&json).map_err(|e| SecretError::Corrupted(e.to_string()))
            }
            Err(keyring::Error::NoEntry) => Ok(Secrets::new()),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }

    fn set(&self, reference: &str, secrets: &Secrets) -> Result<(), SecretError> {
        let entry = self.entry(reference)?;
        if secrets.is_empty() {
            return match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(SecretError::Keyring(e.to_string())),
            };
        }
        let json =
            serde_json::to_string(secrets).map_err(|e| SecretError::Corrupted(e.to_string()))?;
        entry
            .set_password(&json)
            .map_err(|e| SecretError::Keyring(e.to_string()))
    }
}

/// In-memory store for tests and for systems without a keyring (secrets
/// are then kept for the session only).
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, Secrets>>);

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, reference: &str) -> Result<Secrets, SecretError> {
        parse_reference(reference)?;
        Ok(self
            .0
            .lock()
            .expect("secret store lock")
            .get(reference)
            .cloned()
            .unwrap_or_default())
    }

    fn set(&self, reference: &str, secrets: &Secrets) -> Result<(), SecretError> {
        parse_reference(reference)?;
        let mut map = self.0.lock().expect("secret store lock");
        if secrets.is_empty() {
            map.remove(reference);
        } else {
            map.insert(reference.to_owned(), secrets.clone());
        }
        Ok(())
    }
}

/// Uses the keyring when it works, otherwise memory (logged once).
pub fn system_store(service: &str) -> Box<dyn SecretStore> {
    let store = KeyringStore::new(service);
    match store.get(&reference("ferrodb-probe")) {
        Ok(_) => Box::new(store),
        Err(e) => {
            tracing::warn!(error = %e, "system keyring unavailable; secrets are kept in memory for this session");
            Box::new(MemoryStore::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(store: &dyn SecretStore, r: &str) {
        assert!(store.get(r).unwrap().is_empty());
        let s: Secrets = [("password".to_owned(), "p@ss \"quoted\"".to_owned())].into();
        store.set(r, &s).unwrap();
        assert_eq!(store.get(r).unwrap(), s);
        store.delete(r).unwrap();
        assert!(store.get(r).unwrap().is_empty());
        store.delete(r).unwrap();
    }

    #[test]
    fn memory_store() {
        roundtrip(&MemoryStore::new(), "secret://ds-1");
        assert!(MemoryStore::new().get("ds-1").is_err());
    }

    /// Touches the real keyring of the machine running the tests, so it is
    /// opt-in: `FERRODB_TEST_KEYRING=1 cargo test -p ferrodb-secrets`.
    #[test]
    fn system_keyring() {
        if std::env::var_os("FERRODB_TEST_KEYRING").is_none() {
            return;
        }
        roundtrip(
            &KeyringStore::new("FerroDb-test"),
            "secret://keyring-roundtrip",
        );
    }
}
