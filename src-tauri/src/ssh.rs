//! SSH tunnel: a local TCP port forwarded to the database host through an SSH server.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client;
use russh::keys::ssh_key::known_hosts::{HostPatterns, KnownHosts, Marker};
use russh::keys::{Certificate, HashAlg, PrivateKeyWithHashAlg, PublicKey};
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
    /// known_hosts entries for this host: (marker, key); `@cert-authority` and `@revoked` are used for certificates.
    entries: Vec<(Option<Marker>, PublicKey)>,
    /// Why the server key was refused, reported to the UI (with the fingerprint when it can be trusted).
    refused: Arc<Mutex<Option<AppError>>>,
}

/// `host` as known_hosts writes it: bare on port 22, `[host]:port` otherwise.
fn host_port(host: &str, port: u16) -> String {
    if port == 22 { host.to_string() } else { format!("[{host}]:{port}") }
}

/// OpenSSH glob: `*` any run, `?` one character, case-insensitive.
fn glob(pattern: &str, name: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    go(pattern.as_bytes(), name.as_bytes())
}

/// A comma-separated pattern list matches when one pattern does and no `!pattern` does.
// ponytail: hashed names (|1|…) are skipped here; they hold plain host keys, @cert-authority lines are written in clear.
fn patterns_match(patterns: &HostPatterns, name: &str) -> bool {
    let HostPatterns::Patterns(list) = patterns else { return false };
    let negated = list.iter().filter_map(|p| p.strip_prefix('!')).any(|p| glob(p, name));
    !negated && list.iter().any(|p| !p.starts_with('!') && glob(p, name))
}

/// The marked entries (`@cert-authority`, `@revoked`) of known_hosts text that apply to `host:port`.
fn marked_entries(known_hosts: &str, host: &str, port: u16) -> Vec<(Option<Marker>, PublicKey)> {
    let name = host_port(host, port);
    KnownHosts::new(known_hosts)
        .filter_map(Result::ok) // a line this parser does not know is skipped, as by OpenSSH
        .filter(|e| e.marker().is_some() && patterns_match(e.host_patterns(), &name))
        .map(|e| (e.marker().cloned(), e.public_key().clone()))
        .collect()
}

/// A host certificate is trusted when signed by a `@cert-authority` listed for this host, within its validity
/// period, issued for this host name, with no critical option, and neither it nor its CA is `@revoked`.
fn host_cert_trusted(cert: &Certificate, host: &str, entries: &[(Option<Marker>, PublicKey)], now: u64) -> bool {
    let of = |m: Marker| entries.iter().filter(move |(k, _)| *k == Some(m.clone())).map(|(_, key)| key.key_data());
    let cas: Vec<_> = of(Marker::CertAuthority).map(|k| k.fingerprint(HashAlg::Sha256)).collect();
    let revoked = of(Marker::Revoked).any(|k| k == cert.signature_key() || k == cert.public_key());
    cert.cert_type().is_host()
        && !revoked
        && cert.critical_options().is_empty()
        && cert.valid_principals().iter().any(|p| p.eq_ignore_ascii_case(host))
        && cert.validate_at(now, cas.iter()).is_ok()
}

impl client::Handler for HostCheck {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = match key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => key,
            // Only offered when known_hosts lists a CA for this host; no trust-on-first-use for certificates.
            russh::keys::PublicKeyOrCertificate::Certificate(cert) => {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                let ok = host_cert_trusted(cert, &self.host, &self.entries, now);
                if !ok {
                    let ca = cert.signature_key().fingerprint(HashAlg::Sha256).to_string();
                    *self.refused.lock().unwrap() = Some(AppError { detail: Some(ca), ..AppError::new("SSH_HOST_CERT", host_port(&self.host, self.port)) });
                }
                return Ok(ok);
            }
        };
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        if self.trusted.as_deref() == Some(fingerprint.as_str()) {
            return Ok(true);
        }
        // Err = the host is in known_hosts with a *different* key: possible attack.
        let (known, code) = match russh::keys::check_known_hosts(&self.host, self.port, key) {
            Ok(known) => (known, "SSH_HOST_UNKNOWN"),
            Err(_) => (false, "SSH_HOST_CHANGED"),
        };
        *self.refused.lock().unwrap() = Some(AppError { detail: Some(fingerprint), ..AppError::new(code, host_port(&self.host, self.port)) });
        Ok(known)
    }
}

/// Open the SSH session and start forwarding 127.0.0.1:<random port> -> db_host:db_port.
pub async fn open(ssh: &Ssh, secret: Option<&str>, db_host: &str, db_port: u16) -> AppResult<Tunnel> {
    let known_hosts = std::env::home_dir().and_then(|h| std::fs::read_to_string(h.join(".ssh").join("known_hosts")).ok()).unwrap_or_default();
    open_with(ssh, secret, db_host, db_port, &known_hosts).await
}

/// `open` with the known_hosts text given (tests); plain host keys are still checked in ~/.ssh/known_hosts by russh.
async fn open_with(ssh: &Ssh, secret: Option<&str>, db_host: &str, db_port: u16, known_hosts: &str) -> AppResult<Tunnel> {
    let refused = Arc::new(Mutex::new(None));
    let entries = marked_entries(known_hosts, &ssh.host, ssh.port);
    let has_ca = entries.iter().any(|(m, _)| *m == Some(Marker::CertAuthority));
    let check = HostCheck { host: ssh.host.clone(), port: ssh.port, trusted: ssh.host_key.clone(), entries, refused: refused.clone() };
    let mut preferred = russh::Preferred::default();
    if has_ca {
        // As OpenSSH: ask for a host certificate only when an authority is known for this host.
        preferred.host_key_certificates = preferred.key.clone();
    }
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        preferred,
        ..Default::default()
    });

    let connecting = client::connect(config, (ssh.host.as_str(), ssh.port), check);
    let mut session = match tokio::time::timeout(Duration::from_secs(15), connecting).await {
        Err(_) => return Err(AppError::new("SSH_TIMEOUT", format!("{}:{}", ssh.host, ssh.port))),
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(refused.lock().unwrap().take().unwrap_or_else(|| AppError::new("SSH_UNREACHABLE", e.to_string()))),
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

    #[test]
    fn known_hosts_patterns() {
        assert!(glob("*.example.com", "DB.example.com") && glob("db?", "db1") && !glob("db?", "db12"));
        let p = |s: &str| s.parse::<HostPatterns>().unwrap();
        assert!(patterns_match(&p("*.example.com,!bad.example.com"), "db.example.com"));
        assert!(!patterns_match(&p("*.example.com,!bad.example.com"), "bad.example.com"));
        assert!(patterns_match(&p("[db.example.com]:2222"), &host_port("db.example.com", 2222)));
        assert!(!patterns_match(&p("db.example.com"), &host_port("db.example.com", 2222)));
    }

    /// Host certificates made by ssh-keygen (OpenSSH client tools; skipped when missing).
    #[test]
    fn host_certificates() {
        let dir = std::env::temp_dir().join(format!("otaria-hostcert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let keygen = |args: &[&str]| std::process::Command::new("ssh-keygen").args(args).current_dir(&dir).output().map(|o| assert!(o.status.success(), "{o:?}"));
        if keygen(&["-q", "-t", "ed25519", "-N", "", "-f", "ca"]).is_err() {
            eprintln!("ssh-keygen not found: skipped");
            return;
        }
        keygen(&["-q", "-t", "ed25519", "-N", "", "-f", "other_ca"]).unwrap();
        keygen(&["-q", "-t", "ecdsa", "-N", "", "-f", "host"]).unwrap();
        let sign = |name: &str, extra: &[&str]| {
            std::fs::copy(dir.join("host.pub"), dir.join(format!("{name}.pub"))).unwrap();
            let pubf = format!("{name}.pub");
            let mut a = vec!["-q", "-s", "ca", "-I", "test"];
            a.extend_from_slice(extra);
            a.push(&pubf);
            keygen(&a).unwrap();
            Certificate::read_file(&dir.join(format!("{name}-cert.pub"))).unwrap()
        };
        let good = sign("good", &["-h", "-n", "db.example.com,10.0.0.5"]);
        let user = sign("user", &["-n", "db.example.com"]);
        let expired = sign("expired", &["-h", "-n", "db.example.com", "-V", "20200101:20200102"]);
        let ca = std::fs::read_to_string(dir.join("ca.pub")).unwrap();
        let other = std::fs::read_to_string(dir.join("other_ca.pub")).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();

        let kh = format!("@cert-authority *.example.com,10.0.0.* {ca}");
        let e = marked_entries(&kh, "db.example.com", 22);
        assert!(host_cert_trusted(&good, "db.example.com", &e, now));
        assert!(host_cert_trusted(&good, "10.0.0.5", &marked_entries(&kh, "10.0.0.5", 22), now));
        assert!(!host_cert_trusted(&good, "web.example.com", &marked_entries(&kh, "web.example.com", 22), now), "name not in principals");
        assert!(!host_cert_trusted(&user, "db.example.com", &e, now), "user certificate");
        assert!(!host_cert_trusted(&expired, "db.example.com", &e, now), "expired");
        assert!(marked_entries(&kh, "db.example.com", 2222).is_empty(), "other port: the pattern names port 22 only");
        assert!(!host_cert_trusted(&good, "db.example.com", &marked_entries(&format!("@cert-authority * {other}"), "db.example.com", 22), now), "other CA");
        let revoked = format!("{kh}\n@revoked * {ca}");
        assert!(!host_cert_trusted(&good, "db.example.com", &marked_entries(&revoked, "db.example.com", 22), now), "revoked CA");
        // Plain host lines are not CAs.
        assert!(marked_entries(&format!("db.example.com {ca}"), "db.example.com", 22).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A bastion presenting a host certificate: OTARIA_TEST_SSH_HOSTCERT="host:port:user:password:ca.pub path"
    /// (sshd with HostCertificate, signed with -h -n <host>).
    #[tokio::test]
    #[ignore]
    async fn host_certificate_on_test_server() {
        let v: Vec<String> = std::env::var("OTARIA_TEST_SSH_HOSTCERT").expect("set OTARIA_TEST_SSH_HOSTCERT").splitn(5, ':').map(String::from).collect();
        let ssh = Ssh { host: v[0].clone(), port: v[1].parse().unwrap(), user: v[2].clone(), key_path: None, cert_path: None, host_key: None };
        let ca = std::fs::read_to_string(&v[4]).unwrap();
        let name = host_port(&ssh.host, ssh.port);
        open_with(&ssh, Some(&v[3]), "127.0.0.1", 3306, &format!("@cert-authority {name} {ca}")).await.expect("certificate from a known CA");
        // A CA listed for another host only: no certificate asked, the plain key is unknown as before.
        let err = open_with(&ssh, Some(&v[3]), "127.0.0.1", 3306, &format!("@cert-authority other.example {ca}")).await.err().unwrap();
        assert_eq!(err.code, "SSH_HOST_UNKNOWN");
        // The right host, a CA that did not sign it: refused, the CA that did is named.
        let other = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";
        let err = open_with(&ssh, Some(&v[3]), "127.0.0.1", 3306, &format!("@cert-authority {name} {other}")).await.err().unwrap();
        assert_eq!(err.code, "SSH_HOST_CERT");
        assert!(err.detail.unwrap().starts_with("SHA256:"));
    }
}
