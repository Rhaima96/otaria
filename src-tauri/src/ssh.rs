//! SSH tunnel: a local TCP port forwarded to the database host through an SSH server.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::error::{AppError, AppResult};
use crate::profiles::Ssh;

/// Stops forwarding when dropped.
pub struct Tunnel {
    pub port: u16,
    task: JoinHandle<()>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct HostCheck {
    host: String,
    port: u16,
    trusted: Option<String>,
    /// Fingerprint presented by the server, reported to the UI when it is not trusted yet.
    seen: Arc<Mutex<Option<(String, bool)>>>,
}

impl client::Handler for HostCheck {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false); // ponytail: SSH certificates not supported yet
        };
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        if self.trusted.as_deref() == Some(fingerprint.as_str()) {
            return Ok(true);
        }
        // Err = the host is in known_hosts with a *different* key: possible attack.
        let (known, changed) = match russh::keys::check_known_hosts(&self.host, self.port, key) {
            Ok(known) => (known, false),
            Err(_) => (false, true),
        };
        *self.seen.lock().unwrap() = Some((fingerprint, changed));
        Ok(known)
    }
}

/// Open the SSH session and start forwarding 127.0.0.1:<random port> -> db_host:db_port.
pub async fn open(ssh: &Ssh, secret: Option<&str>, db_host: &str, db_port: u16) -> AppResult<Tunnel> {
    let seen = Arc::new(Mutex::new(None));
    let check = HostCheck {
        host: ssh.host.clone(),
        port: ssh.port,
        trusted: ssh.host_key.clone(),
        seen: seen.clone(),
    };
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });

    let connecting = client::connect(config, (ssh.host.as_str(), ssh.port), check);
    let mut session = match tokio::time::timeout(Duration::from_secs(15), connecting).await {
        Err(_) => return Err(AppError::new("SSH_TIMEOUT", format!("{}:{}", ssh.host, ssh.port))),
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            return Err(match seen.lock().unwrap().take() {
                Some((fp, true)) => AppError { detail: Some(fp), ..AppError::new("SSH_HOST_CHANGED", e.to_string()) },
                Some((fp, false)) => AppError { detail: Some(fp), ..AppError::new("SSH_HOST_UNKNOWN", e.to_string()) },
                None => AppError::new("SSH_UNREACHABLE", e.to_string()),
            });
        }
    };

    let ssh_err = |e: russh::Error| AppError::new("SSH", e.to_string());
    let key_path = ssh.key_path.as_deref().filter(|p| !p.trim().is_empty()).map(home);
    let auth = match key_path.as_deref() {
        Some(path) => {
            let key = russh::keys::load_secret_key(path, secret)
                .map_err(|e| AppError::new("SSH_KEY", e.to_string()))?;
            match cert_file(path, ssh.cert_path.as_deref().map(home).as_deref()) {
                Some(cert) => {
                    let cert = russh::keys::load_openssh_certificate(&cert).map_err(|e| AppError { detail: Some(cert.clone()), ..AppError::new("SSH_CERT", e.to_string()) })?;
                    session.authenticate_openssh_cert(&ssh.user, Arc::new(key), cert).await
                }
                None => {
                    let hash = session.best_supported_rsa_hash().await.map_err(ssh_err)?.flatten();
                    session.authenticate_publickey(&ssh.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await
                }
            }
        }
        None => session.authenticate_password(&ssh.user, secret.unwrap_or_default()).await,
    }
    .map_err(ssh_err)?;
    if !auth.success() {
        return Err(AppError::new("SSH_AUTH", format!("{}@{}", ssh.user, ssh.host)));
    }

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let session = Arc::new(session);
    let (db_host, db_port) = (db_host.to_string(), db_port);
    let task = tokio::spawn(async move {
        while let Ok((mut socket, peer)) = listener.accept().await {
            let (session, db_host) = (session.clone(), db_host.clone());
            tokio::spawn(async move {
                let channel = session
                    .channel_open_direct_tcpip(db_host, db_port.into(), "127.0.0.1", peer.port().into())
                    .await;
                if let Ok(channel) = channel {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                }
            });
        }
    });
    Ok(Tunnel { port, task })
}

/// The certificate to send with key `key`: the one chosen, else `<key>-cert.pub` when it exists.
fn cert_file(key: &str, chosen: Option<&str>) -> Option<String> {
    match chosen.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => Some(c.to_string()),
        None => Some(format!("{key}-cert.pub")).filter(|c| std::path::Path::new(c).is_file()),
    }
}

/// `~/…` → the home directory (paths typed by hand, as in a shell).
fn home(path: &str) -> String {
    let path = path.trim();
    match (path.strip_prefix("~/"), std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })) {
        (Some(rest), Some(h)) => std::path::Path::new(&h).join(rest).to_string_lossy().into_owned(),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_and_certificate_paths() {
        let h = std::env::var("HOME").unwrap();
        assert_eq!(home("~/.ssh/id_ed25519"), format!("{h}/.ssh/id_ed25519"));
        assert_eq!(home(" /abs/key "), "/abs/key");
        assert_eq!(cert_file("/k", Some(" /c-cert.pub ")).as_deref(), Some("/c-cert.pub"));
        assert_eq!(cert_file("/nonexistent/k", None), None);
    }
}
