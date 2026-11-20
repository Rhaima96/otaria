//! Opening MariaDB connections from a profile: TLS, socket, SSH tunnel, server facts.

use std::time::Duration;

use mysql_async::prelude::Queryable;
use mysql_async::{ClientIdentity, Conn, Opts, OptsBuilder, Pool, PoolConstraints, PoolOpts, SslOpts};
use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::profiles::{Profile, TlsMode};
use crate::ssh::{self, Tunnel};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Facts shown right after connecting; `is_mariadb` drives the MySQL warning banner.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub version: String,
    pub version_comment: String,
    pub is_mariadb: bool,
    pub current_user: String,
    pub connection_id: u32,
    /// Negotiated TLS cipher, None when the link is not encrypted.
    pub tls_cipher: Option<String>,
    pub via_ssh: bool,
}

/// TLS options for a mode. `sni_host` is the real server name when going through an SSH tunnel.
fn ssl_opts(profile: &Profile, mode: TlsMode, sni_host: Option<&str>) -> Option<SslOpts> {
    let tls = &profile.tls;
    let mut opts = match mode {
        TlsMode::Disabled => return None,
        TlsMode::Preferred | TlsMode::Required => SslOpts::default().with_danger_accept_invalid_certs(true),
        TlsMode::VerifyCa => SslOpts::default().with_danger_skip_domain_validation(true),
        TlsMode::VerifyIdentity => SslOpts::default(),
    };
    if let Some(ca) = tls.ca.as_deref().filter(|s| !s.is_empty()) {
        opts = opts.with_root_certs(vec![std::path::PathBuf::from(ca).into()]);
    }
    if let (Some(cert), Some(key)) = (tls.cert.as_deref(), tls.key.as_deref()) {
        if !cert.is_empty() && !key.is_empty() {
            let id = ClientIdentity::new(std::path::PathBuf::from(cert).into(), std::path::PathBuf::from(key).into());
            opts = opts.with_client_identity(Some(id));
        }
    }
    if let Some(host) = sni_host {
        opts = opts.with_danger_tls_hostname_override(Some(host.to_string()));
    }
    Some(opts)
}

/// `tag`: this session's own value of the `otaria_session` connection attribute (admin.rs finds its connections by it).
fn opts(profile: &Profile, password: Option<&str>, tunnel: Option<&Tunnel>, mode: TlsMode, tag: &str) -> Opts {
    let socket = profile.socket.as_deref().filter(|s| !s.is_empty() && tunnel.is_none());
    let (host, port) = match tunnel {
        Some(t) => ("127.0.0.1".to_string(), t.port),
        None => (profile.host.clone(), profile.port),
    };
    let mode = if socket.is_some() { TlsMode::Disabled } else { mode };
    let sni = tunnel.map(|_| profile.host.as_str());
    OptsBuilder::default()
        .ip_or_hostname(host)
        .tcp_port(port)
        .socket(socket)
        // Without this, mysql_async silently switches localhost connections to the server's socket.
        .prefer_socket(false)
        // Affected rows = matched rows, so a grid UPDATE that changes nothing still counts as 1 (edit.rs checks it).
        .client_found_rows(true)
        // Shown by performance_schema.session_connect_attrs (and SHOW PROCESSLIST tools that read it).
        // Over TLS too thanks to the patched mysql_async (vendor/, see opts/mod.rs "Otaria patch").
        .connect_attribute("program_name", "otaria")
        .connect_attribute("otaria_session", tag)
        .user(Some(profile.user.clone()))
        .pass(password.map(str::to_string))
        .db_name(profile.database.clone().filter(|d| !d.is_empty()))
        .ssl_opts(ssl_opts(profile, mode, sni))
        .pool_opts(PoolOpts::default().with_constraints(PoolConstraints::new(0, 8).expect("valid constraints")))
        .into()
}

async fn with_timeout<T>(fut: impl std::future::Future<Output = Result<T, mysql_async::Error>>) -> AppResult<T> {
    match tokio::time::timeout(CONNECT_TIMEOUT, fut).await {
        Ok(r) => r.map_err(AppError::from),
        Err(_) => Err(AppError::new("DB_TIMEOUT", format!("{}s", CONNECT_TIMEOUT.as_secs()))),
    }
}

/// `VERSION()` ("8.0.46", "5.7.44-log", "12.3.3-MariaDB-…") is at least `min`.
pub fn version_at_least(version: &str, min: (u32, u32, u32)) -> bool {
    let mut parts = version.split(|c: char| !c.is_ascii_digit()).map(|p| p.parse::<u32>().unwrap_or(0));
    let v = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    v >= min
}

async fn server_info(conn: &mut Conn, via_ssh: bool) -> AppResult<ServerInfo> {
    let (version, version_comment, current_user, connection_id): (String, String, String, u32) = conn
        .query_first("SELECT VERSION(), @@version_comment, CURRENT_USER(), CONNECTION_ID()")
        .await?
        .ok_or_else(|| AppError::new("DB", "empty server info"))?;
    let cipher: Option<(String, String)> = conn.query_first("SHOW SESSION STATUS LIKE 'Ssl_cipher'").await?;
    Ok(ServerInfo {
        is_mariadb: version.to_ascii_lowercase().contains("mariadb"),
        version,
        version_comment,
        current_user,
        connection_id,
        tls_cipher: cipher.map(|(_, c)| c).filter(|c| !c.is_empty()),
        via_ssh,
    })
}

/// An open, validated connection pool (plus the SSH tunnel it needs, if any).
pub struct Session {
    pub pool: Pool,
    pub info: ServerInfo,
    /// SQL editor tabs, each holding its own connection (see query.rs).
    pub editors: crate::query::Editors,
    /// For external clients (mariadb-binlog --flashback).
    pub cli: crate::binlog::Cli,
    /// Value of the `otaria_session` connection attribute on every connection of `pool`.
    pub tag: String,
    _tunnel: Option<Tunnel>,
}

impl Session {
    pub async fn close(self) {
        // Editor connections must go back to the pool first, or disconnect() waits for them forever.
        drop(self.editors);
        let _ = self.pool.disconnect().await;
    }
}

/// Connect, check the credentials and read the server facts.
/// With TLS "preferred", a server without TLS support falls back to a plain connection.
pub async fn open(profile: &Profile, password: Option<&str>, ssh_secret: Option<&str>) -> AppResult<Session> {
    let tunnel = match &profile.ssh {
        Some(s) => Some(ssh::open(s, ssh_secret, &profile.host, profile.port).await?),
        None => None,
    };
    let tag = uuid::Uuid::new_v4().to_string();
    let try_open = |mode| {
        let pool = Pool::new(opts(profile, password, tunnel.as_ref(), mode, &tag));
        async move {
            let mut conn = with_timeout(pool.get_conn()).await?;
            let info = server_info(&mut conn, profile.ssh.is_some()).await?;
            drop(conn);
            Ok::<_, AppError>((pool, info))
        }
    };
    let (pool, info, mode) = match try_open(profile.tls.mode).await {
        // "If available" checks no certificate, so a plain link loses no protection it had: fall back when the
        // server has no TLS, or none in common with ours (MySQL 5.7).
        Err(e) if profile.tls.mode == TlsMode::Preferred && (e.code == "TLS_NOT_SUPPORTED" || e.code == "TLS_INCOMPATIBLE") => {
            let (pool, info) = try_open(TlsMode::Disabled).await?;
            (pool, info, TlsMode::Disabled)
        }
        r => {
            let (pool, info) = r?;
            (pool, info, profile.tls.mode)
        }
    };
    let socket = profile.socket.clone().filter(|s| !s.is_empty() && tunnel.is_none());
    let cli = crate::binlog::Cli {
        host: if tunnel.is_some() { "127.0.0.1".into() } else { profile.host.clone() },
        port: tunnel.as_ref().map_or(profile.port, |t| t.port),
        tls: if socket.is_some() { TlsMode::Disabled } else { mode },
        socket,
        user: profile.user.clone(),
        password: password.map(str::to_string),
        ca: profile.tls.ca.clone(),
        cert: profile.tls.cert.clone(),
        key: profile.tls.key.clone(),
    };
    Ok(Session { pool, info, editors: Default::default(), cli, tag, _tunnel: tunnel })
}

#[cfg(test)]
mod tests {
    #[test]
    fn compares_versions() {
        assert!(super::version_at_least("8.0.26", (8, 0, 26)) && super::version_at_least("8.4.13", (8, 0, 26)));
        assert!(!super::version_at_least("8.0.25", (8, 0, 26)) && !super::version_at_least("5.7.44-log", (8, 0, 18)));
        assert!(super::version_at_least("12.3.3-MariaDB-ubu2404", (10, 11, 0)));
    }

    use super::*;
    use crate::profiles::Tls;

    fn profile(mode: TlsMode) -> Profile {
        Profile {
            id: "t".into(),
            name: "t".into(),
            color: None,
            host: "db.example.com".into(),
            port: 3306,
            user: "u".into(),
            database: None,
            socket: None,
            save_password: false,
            tls: Tls { mode, ..Default::default() },
            ssh: None,
        }
    }

    #[test]
    fn tls_modes_map_to_ssl_opts() {
        let p = profile(TlsMode::Disabled);
        assert!(ssl_opts(&p, TlsMode::Disabled, None).is_none());
        let req = ssl_opts(&p, TlsMode::Required, None).unwrap();
        assert!(req.accept_invalid_certs());
        let ca = ssl_opts(&p, TlsMode::VerifyCa, None).unwrap();
        assert!(!ca.accept_invalid_certs() && ca.skip_domain_validation());
        let full = ssl_opts(&p, TlsMode::VerifyIdentity, None).unwrap();
        assert!(!full.accept_invalid_certs() && !full.skip_domain_validation());
    }

    #[test]
    fn socket_disables_tls() {
        let mut p = profile(TlsMode::VerifyIdentity);
        p.socket = Some("/tmp/mysql.sock".into());
        let o = opts(&p, None, None, p.tls.mode, "t");
        assert_eq!(o.connect_attributes().unwrap()["program_name"], "otaria");
        assert!(o.ssl_opts().is_none());
        assert_eq!(o.socket(), Some("/tmp/mysql.sock"));
    }

    /// Live check against a real server: OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn connects_to_test_server() {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let parts: Vec<&str> = spec.splitn(4, ':').collect();
        let mut p = profile(TlsMode::Preferred);
        p.host = parts[0].into();
        p.port = parts[1].parse().unwrap();
        p.user = parts[2].into();
        let session = open(&p, Some(parts[3]), None).await.expect("connect");
        assert!(session.info.is_mariadb, "{:?}", session.info);
        // MariaDB >= 11.4 has TLS on by default, so "preferred" must end up encrypted.
        assert!(session.info.tls_cipher.is_some(), "{:?}", session.info);
        session.close().await;

        p.tls.mode = TlsMode::VerifyIdentity; // self-signed server cert: must be rejected
        let e = open(&p, Some(parts[3]), None).await.err().expect("must fail");
        assert_eq!(e.code, "TLS", "{e:?}");
        p.tls.mode = TlsMode::Preferred;

        let err = open(&p, Some("wrong-password"), None).await.err().expect("must fail");
        assert_eq!(err.code, "DB_ACCESS_DENIED");
    }

    /// TLS checks against a server whose certificate, signed by the CA in `ca_path`, names only
    /// `localhost`: OTARIA_TEST_TLS_DB="127.0.0.1:port:user:ca_path" (passwordless user).
    /// "Verify CA" must ignore the name (patched mysql_async), "verify identity" must not.
    #[tokio::test]
    #[ignore]
    async fn verify_ca_ignores_host_name() {
        let spec = std::env::var("OTARIA_TEST_TLS_DB").expect("set OTARIA_TEST_TLS_DB");
        let parts: Vec<&str> = spec.splitn(4, ':').collect();
        let mut p = profile(TlsMode::VerifyCa);
        p.host = parts[0].into();
        p.port = parts[1].parse().unwrap();
        p.user = parts[2].into();
        p.tls.ca = Some(parts[3].into());
        open(&p, None, None).await.expect("verify-ca by IP").close().await;
        p.tls.mode = TlsMode::VerifyIdentity;
        let e = open(&p, None, None).await.err().expect("name mismatch must fail");
        assert_eq!(e.code, "TLS", "{e:?}");
        p.host = "localhost".into();
        open(&p, None, None).await.expect("verify-identity by name").close().await;
    }

    /// SSH tunnel: OTARIA_TEST_SSH="ssh_host:ssh_port:user:password:db_host" plus OTARIA_TEST_DB for the DB login.
    #[tokio::test]
    #[ignore]
    async fn connects_through_ssh_tunnel() {
        let ssh: Vec<String> = std::env::var("OTARIA_TEST_SSH").expect("set OTARIA_TEST_SSH").splitn(5, ':').map(String::from).collect();
        let dbs: Vec<String> = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB").splitn(4, ':').map(String::from).collect();
        let mut p = profile(TlsMode::Preferred);
        p.host = ssh[4].clone(); // as seen from the bastion
        p.port = 3306;
        p.user = dbs[2].clone();
        p.ssh = Some(crate::profiles::Ssh {
            host: ssh[0].clone(),
            port: ssh[1].parse().unwrap(),
            user: ssh[2].clone(),
            key_path: None,
            cert_path: None,
            host_key: None,
        });

        // First contact: unknown host key -> the UI gets the fingerprint to confirm.
        let err = open(&p, Some(&dbs[3]), Some(&ssh[3])).await.err().expect("unknown host must fail");
        assert_eq!(err.code, "SSH_HOST_UNKNOWN");
        let fingerprint = err.detail.expect("fingerprint");
        assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");

        // After the user trusts it, the tunnel works.
        p.ssh.as_mut().unwrap().host_key = Some(fingerprint);
        let session = open(&p, Some(&dbs[3]), Some(&ssh[3])).await.expect("connect via ssh");
        assert!(session.info.via_ssh && session.info.is_mariadb, "{:?}", session.info);
        session.close().await;

        let bad = open(&p, Some(&dbs[3]), Some("wrong")).await.err().expect("bad ssh password");
        assert_eq!(bad.code, "SSH_AUTH");
    }

    /// SSH user certificate: OTARIA_TEST_SSH as above (its password unused) and OTARIA_TEST_SSH_KEY, a private key
    /// whose `<key>-cert.pub` is signed by a CA the bastion trusts (TrustedUserCAKeys); the key alone is not authorized.
    #[tokio::test]
    #[ignore]
    async fn connects_with_ssh_certificate() {
        let ssh: Vec<String> = std::env::var("OTARIA_TEST_SSH").expect("set OTARIA_TEST_SSH").splitn(5, ':').map(String::from).collect();
        let key = std::env::var("OTARIA_TEST_SSH_KEY").expect("set OTARIA_TEST_SSH_KEY");
        let dbs: Vec<String> = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB").splitn(4, ':').map(String::from).collect();
        let mut p = profile(TlsMode::Preferred);
        p.host = ssh[4].clone();
        p.port = 3306;
        p.user = dbs[2].clone();
        let mut tunnel = crate::profiles::Ssh { host: ssh[0].clone(), port: ssh[1].parse().unwrap(), user: ssh[2].clone(), key_path: Some(key.clone()), cert_path: None, host_key: None };
        p.ssh = Some(tunnel.clone());
        let fingerprint = open(&p, Some(&dbs[3]), None).await.err().expect("unknown host").detail.expect("fingerprint");
        tunnel.host_key = Some(fingerprint);

        // <key>-cert.pub found next to the key, then the same file chosen explicitly.
        p.ssh = Some(tunnel.clone());
        open(&p, Some(&dbs[3]), None).await.expect("certificate found next to the key").close().await;
        p.ssh = Some(crate::profiles::Ssh { cert_path: Some(format!("{key}-cert.pub")), ..tunnel.clone() });
        open(&p, Some(&dbs[3]), None).await.expect("certificate chosen").close().await;

        // Without the certificate (a copy of the key elsewhere), the key alone is refused.
        let dir = std::env::temp_dir().join(format!("otaria-ssh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lone = dir.join("id");
        std::fs::copy(&key, &lone).unwrap();
        p.ssh = Some(crate::profiles::Ssh { key_path: Some(lone.to_string_lossy().into_owned()), ..tunnel.clone() });
        assert_eq!(open(&p, Some(&dbs[3]), None).await.err().expect("key alone refused").code, "SSH_AUTH");
        p.ssh = Some(crate::profiles::Ssh { cert_path: Some("/nonexistent-cert.pub".into()), ..tunnel });
        assert_eq!(open(&p, Some(&dbs[3]), None).await.err().expect("missing certificate").code, "SSH_CERT");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
