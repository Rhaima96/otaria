//! Saved connection profiles (JSON in the app config dir) and their secrets (OS keychain).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// Tag colour shown on tabs, e.g. "red" for production.
    #[serde(default)]
    pub color: Option<String>,
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub database: Option<String>,
    /// Unix socket path; when set, host/port are ignored (and TLS is not used).
    #[serde(default)]
    pub socket: Option<String>,
    /// Keep the DB password (and SSH secret) in the OS keychain.
    #[serde(default)]
    pub save_password: bool,
    #[serde(default)]
    pub tls: Tls,
    #[serde(default)]
    pub ssh: Option<Ssh>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tls {
    pub mode: TlsMode,
    /// CA certificate (PEM) to verify the server.
    #[serde(default)]
    pub ca: Option<String>,
    /// Client certificate chain + private key (PEM), for X509 accounts.
    #[serde(default)]
    pub cert: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    Disabled,
    /// Encrypt when the server supports it, else plain (like the mariadb CLI default).
    #[default]
    Preferred,
    /// Encrypt, don't check the certificate.
    Required,
    /// Encrypt and check the certificate chain, not the host name.
    VerifyCa,
    /// Encrypt and check certificate and host name.
    VerifyIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Ssh {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Private key file; when empty, password authentication is used.
    #[serde(default)]
    pub key_path: Option<String>,
    /// OpenSSH user certificate signed by a CA (`…-cert.pub`), sent with the key. Empty: `<key>-cert.pub`
    /// is used when it exists, as OpenSSH does.
    #[serde(default)]
    pub cert_path: Option<String>,
    /// SHA256 fingerprint the user accepted (trust on first use), besides ~/.ssh/known_hosts.
    #[serde(default)]
    pub host_key: Option<String>,
}

/// Profiles file + keychain access.
pub struct Store {
    path: PathBuf,
}

const KEYCHAIN_SERVICE: &str = "dev.otaria.app";

#[derive(Clone, Copy)]
pub enum Secret {
    /// MariaDB password.
    Db,
    /// SSH password or private-key passphrase.
    Ssh,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self { path: dir.join("connections.json") }
    }

    pub fn list(&self) -> AppResult<Vec<Profile>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| AppError::new("PROFILES_CORRUPT", format!("{}: {e}", self.path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get(&self, id: &str) -> AppResult<Profile> {
        self.list()?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| AppError::new("PROFILE_NOT_FOUND", id))
    }

    /// Insert or replace by id (a new id is assigned when empty). Returns the stored profile.
    pub fn upsert(&self, mut profile: Profile) -> AppResult<Profile> {
        if profile.id.is_empty() {
            profile.id = uuid::Uuid::new_v4().to_string();
        }
        let mut all = self.list()?;
        match all.iter_mut().find(|p| p.id == profile.id) {
            Some(p) => *p = profile.clone(),
            None => all.push(profile.clone()),
        }
        self.write(&all)?;
        Ok(profile)
    }

    pub fn delete(&self, id: &str) -> AppResult<()> {
        let mut all = self.list()?;
        all.retain(|p| p.id != id);
        self.write(&all)?;
        self.set_secret(id, Secret::Db, None)?;
        self.set_secret(id, Secret::Ssh, None)
    }

    /// Atomic write (temp file + rename) so a crash never leaves a half-written file.
    fn write(&self, all: &[Profile]) -> AppResult<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(all).expect("profiles serialize"))?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    fn entry(id: &str, kind: Secret) -> keyring::Result<keyring::Entry> {
        let account = match kind {
            Secret::Db => format!("{id}/db"),
            Secret::Ssh => format!("{id}/ssh"),
        };
        keyring::Entry::new(KEYCHAIN_SERVICE, &account)
    }

    /// `None` removes the secret.
    pub fn set_secret(&self, id: &str, kind: Secret, value: Option<&str>) -> AppResult<()> {
        let entry = Self::entry(id, kind)?;
        match value {
            Some(v) => entry.set_password(v)?,
            None => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(e.into()),
            },
        }
        Ok(())
    }

    pub fn get_secret(&self, id: &str, kind: Secret) -> AppResult<Option<String>> {
        match Self::entry(id, kind)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Profile {
        Profile {
            id: String::new(),
            name: "local".into(),
            color: None,
            host: "127.0.0.1".into(),
            port: 3306,
            user: "root".into(),
            database: None,
            socket: None,
            save_password: false,
            tls: Tls::default(),
            ssh: None,
        }
    }

    #[test]
    fn upsert_assigns_id_and_replaces() {
        let dir = std::env::temp_dir().join(format!("otaria-test-{}", uuid::Uuid::new_v4()));
        let store = Store::new(dir.clone());
        assert!(store.list().unwrap().is_empty());

        let saved = store.upsert(sample()).unwrap();
        assert!(!saved.id.is_empty());

        let mut renamed = saved.clone();
        renamed.name = "renamed".into();
        store.upsert(renamed).unwrap();
        let all = store.list().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "renamed");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_json_without_new_fields_still_loads() {
        let p: Profile =
            serde_json::from_str(r#"{"name":"a","host":"h","port":3306,"user":"u"}"#).unwrap();
        assert_eq!(p.tls.mode, TlsMode::Preferred);
        assert!(p.ssh.is_none());
    }
}
