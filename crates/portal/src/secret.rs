//! Where the refresh token is kept: the operating system's credential store (Windows Credential
//! Manager, the macOS Keychain), keyed by the portal's address, never a file. On other systems
//! there is no store this app can reach without a new system dependency, so the token is kept for
//! this run only ([`MemoryStore`]) and the person logs in again next time ([`SecretStore::persistent`]
//! says which one is in use).

use std::sync::{Mutex, PoisonError};

/// A place for one secret.
pub trait SecretStore: Send + Sync {
    /// The secret kept, if any.
    fn load(&self) -> Result<Option<String>, String>;
    fn save(&self, secret: &str) -> Result<(), String>;
    /// Forget the secret (nothing kept is fine).
    fn clear(&self) -> Result<(), String>;
    /// The secret outlives this run.
    fn persistent(&self) -> bool;
}

/// A secret kept in memory only.
#[derive(Default)]
pub struct MemoryStore(Mutex<Option<String>>);

impl SecretStore for MemoryStore {
    fn load(&self) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap_or_else(PoisonError::into_inner).clone())
    }
    fn save(&self, secret: &str) -> Result<(), String> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(secret.to_string());
        Ok(())
    }
    fn clear(&self) -> Result<(), String> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }
    fn persistent(&self) -> bool {
        false
    }
}

/// The credential store entry for the portal at `base_url`.
#[cfg(any(windows, target_os = "macos"))]
pub struct SystemStore {
    entry: keyring::Entry,
}

/// The service name entries are filed under.
#[cfg(any(windows, target_os = "macos"))]
const SERVICE: &str = "FigIran portal";

#[cfg(any(windows, target_os = "macos"))]
impl SystemStore {
    pub fn new(base_url: &str) -> Result<Self, String> {
        keyring::Entry::new(SERVICE, base_url).map(|entry| Self { entry }).map_err(|e| e.to_string())
    }
}

#[cfg(any(windows, target_os = "macos"))]
impl SecretStore for SystemStore {
    fn load(&self) -> Result<Option<String>, String> {
        match self.entry.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    fn save(&self, secret: &str) -> Result<(), String> {
        self.entry.set_password(secret).map_err(|e| e.to_string())
    }
    fn clear(&self) -> Result<(), String> {
        match self.entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
    fn persistent(&self) -> bool {
        true
    }
}

/// The best store this system has for the portal at `base_url`.
pub fn system_store(base_url: &str) -> Box<dyn SecretStore> {
    #[cfg(any(windows, target_os = "macos"))]
    if let Ok(store) = SystemStore::new(base_url) {
        return Box::new(store);
    }
    let _ = base_url;
    Box::new(MemoryStore::default())
}
