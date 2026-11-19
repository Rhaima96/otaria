//! Binary log reader (MariaDB only): files from `SHOW BINARY LOGS`, events from `SHOW BINLOG EVENTS`,
//! read one page at a time; flashback (undo SQL for a range of row events) through the external
//! `mariadb-binlog` client. Nothing is run here: the undo script opens in the SQL editor.

use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use serde::Serialize;

use crate::designer::literal;
use crate::error::{AppError, AppResult};
use crate::profiles::TlsMode;

/// How an external client reaches the session's server: the same host and port (the SSH tunnel's local
/// port when there is one), user, password and TLS mode as the session's own connections.
#[derive(Clone)]
pub struct Cli {
    pub host: String,
    pub port: u16,
    pub socket: Option<String>,
    pub user: String,
    /// Passed to the child process through MYSQL_PWD, never on the command line (visible in `ps`).
    pub password: Option<String>,
    /// The mode the session ended up with (Preferred may have fallen back to Disabled).
    pub tls: TlsMode,
    pub ca: Option<String>,
    pub cert: Option<String>,
    pub key: Option<String>,
}

/// `mariadb-binlog` arguments that print the undo of the events in [start, stop) of `file`.
pub fn flashback_args(cli: &Cli, file: &str, start: u64, stop: u64) -> Vec<String> {
    let mut a = vec!["--no-defaults".to_string()];
    match &cli.socket {
        Some(s) => a.push(format!("--socket={s}")),
        None => a.extend([format!("--host={}", cli.host), format!("--port={}", cli.port), "--protocol=TCP".into()]),
    }
    a.push(format!("--user={}", cli.user));
    let some = |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_string);
    match cli.tls {
        TlsMode::Disabled => a.push("--skip-ssl".into()),
        mode => {
            a.push("--ssl".into());
            // ponytail: VerifyCa also skips the name check here (the client has no CA-only mode), and through a
            // tunnel VerifyIdentity would check 127.0.0.1: both use the CA without the name check in that case.
            let verify_name = mode == TlsMode::VerifyIdentity && cli.host != "127.0.0.1";
            a.push(if verify_name { "--ssl-verify-server-cert" } else { "--skip-ssl-verify-server-cert" }.into());
            if let Some(ca) = some(&cli.ca) {
                a.push(format!("--ssl-ca={ca}"));
            }
            if let (Some(cert), Some(key)) = (some(&cli.cert), some(&cli.key)) {
                a.extend([format!("--ssl-cert={cert}"), format!("--ssl-key={key}")]);
            }
        }
    }
    a.extend([
        "--read-from-remote-server".into(),
        "--flashback".into(),
        // Decoded rows as comments next to each BINLOG statement, so the script can be read before running it.
        "--verbose".into(),
        format!("--start-position={start}"),
        format!("--stop-position={stop}"),
        file.to_string(),
    ]);
    a
}

/// `mariadb-binlog` is not bundled: look in PATH, then where installers put it (a GUI app on macOS
/// does not get the shell's PATH).
fn find_tool() -> Option<std::path::PathBuf> {
    let exe = if cfg!(windows) { "mariadb-binlog.exe" } else { "mariadb-binlog" };
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/usr/local/mariadb/bin", "/opt/local/bin"].map(Into::into));
    if cfg!(windows) {
        // C:\Program Files\MariaDB 11.4\bin and the like.
        for root in ["C:\\Program Files", "C:\\Program Files (x86)"] {
            if let Ok(entries) = std::fs::read_dir(root) {
                dirs.extend(entries.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("MariaDB")).map(|e| e.path().join("bin")));
            }
        }
    }
    dirs.into_iter().map(|d| d.join(exe)).find(|p| p.is_file())
}

/// Undo SQL for the events in [start, stop) of `file`, from `mariadb-binlog --flashback`.
/// Only row events can be reversed (binlog_format ROW, binlog_row_image FULL); DDL is skipped by the tool.
pub async fn flashback(pool: &Pool, cli: &Cli, file: &str, start: u64, stop: u64) -> AppResult<String> {
    file_size(&mut pool.get_conn().await?, file).await?;
    if start >= stop {
        return Err(AppError::new("INVALID_RANGE", format!("{start}..{stop}")));
    }
    let tool = find_tool().ok_or_else(|| AppError::new("BINLOG_TOOL_MISSING", "mariadb-binlog".to_string()))?;
    let mut cmd = std::process::Command::new(&tool);
    cmd.args(flashback_args(cli, file, start, stop));
    match &cli.password {
        Some(p) => cmd.env("MYSQL_PWD", p),
        None => cmd.env_remove("MYSQL_PWD"),
    };
    let out = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .map_err(|e| AppError::new("BINLOG_TOOL", e.to_string()))?
        .map_err(|e| AppError::new("BINLOG_TOOL", format!("{}: {e}", tool.display())))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(AppError { detail: Some(err.clone()), ..AppError::new("BINLOG_TOOL", err) });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BinlogFile {
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    /// ROW | STATEMENT | MIXED
    pub format: String,
    /// Oldest first; the last one is the file being written.
    pub files: Vec<BinlogFile>,
    /// `@@gtid_binlog_pos`: last GTID written per domain.
    pub gtid: String,
}

/// `BINLOG_OFF` when the server does not write a binary log (SHOW BINARY LOGS would fail with 1381).
pub async fn state(pool: &Pool) -> AppResult<State> {
    let mut conn = pool.get_conn().await?;
    let (on, format, gtid): (bool, String, String) =
        conn.query_first("SELECT @@log_bin, @@binlog_format, @@gtid_binlog_pos").await?.unwrap_or_default();
    if !on {
        return Err(AppError::new("BINLOG_OFF", String::new()));
    }
    let rows: Vec<mysql_async::Row> = conn.query("SHOW BINARY LOGS").await?;
    let files = rows.into_iter().map(|mut r| BinlogFile { name: r.take(0).unwrap_or_default(), size: r.take(1).unwrap_or_default() }).collect();
    Ok(State { format, files, gtid })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub pos: u64,
    /// Query, Gtid, Annotate_rows (the statement behind row events), Write_rows_v1, Xid…
    pub kind: String,
    pub server_id: u32,
    pub end_pos: u64,
    pub info: String,
}

/// SHOW BINLOG EVENTS takes no parameters: the file name is checked against SHOW BINARY LOGS first,
/// then quoted. `from`: byte position to start at (must be an event start); `offset` then counts events.
pub fn events_sql(file: &str, from: Option<u64>, offset: u64, limit: u64) -> String {
    let from = from.map(|p| format!(" FROM {p}")).unwrap_or_default();
    format!("SHOW BINLOG EVENTS IN {}{from} LIMIT {offset}, {limit}", literal(file))
}

/// Size of `file` from SHOW BINARY LOGS, `NOT_FOUND` when the server has no such file.
async fn file_size(conn: &mut mysql_async::Conn, file: &str) -> AppResult<u64> {
    let files: Vec<(String, u64)> = conn.query_map("SHOW BINARY LOGS", |mut r: mysql_async::Row| (r.take(0).unwrap_or_default(), r.take(1).unwrap_or_default())).await?;
    files.into_iter().find(|(n, _)| n == file).map(|(_, size)| size).ok_or_else(|| AppError::new("NOT_FOUND", file.to_string()))
}

/// Start of the event that contains byte `pos` of `file`: the last event start at or before it.
// ponytail: reads the event list from the top (5000 per page), only when the position typed is not an event start.
async fn event_start(conn: &mut mysql_async::Conn, file: &str, pos: u64) -> AppResult<Option<u64>> {
    let mut best = None;
    for page in 0.. {
        let starts: Vec<u64> = conn.query_map(events_sql(file, None, page * 5000, 5000), |r: mysql_async::Row| r.get::<u64, _>(1).unwrap_or_default()).await?;
        best = starts.iter().copied().filter(|&p| p <= pos).last().or(best);
        if starts.len() < 5000 || starts.last().is_some_and(|&p| p > pos) {
            break;
        }
    }
    Ok(best)
}

/// `from` that is not an event start is moved back to the start of the event containing it (the first
/// event returned says where the page starts); past the end of the file, `BINLOG_POSITION`.
pub async fn events(pool: &Pool, file: &str, from: Option<u64>, offset: u64, limit: u64) -> AppResult<Vec<Event>> {
    let mut conn = pool.get_conn().await?;
    let size = file_size(&mut conn, file).await?;
    let limit = limit.clamp(1, 5000);
    let rows: Vec<mysql_async::Row> = match conn.query(events_sql(file, from, offset, limit)).await {
        Ok(rows) => rows,
        // 1220 "Wrong offset": the position is not where an event starts.
        Err(mysql_async::Error::Server(e)) if e.code == 1220 && from.is_some() => {
            let pos = from.unwrap_or(0);
            match event_start(&mut conn, file, pos).await? {
                Some(start) if pos < size => conn.query(events_sql(file, Some(start), offset, limit)).await?,
                _ => return Err(AppError { detail: Some(file.to_string()), ..AppError::new("BINLOG_POSITION", pos.to_string()) }),
            }
        }
        Err(e) => return Err(e.into()),
    };
    Ok(rows
        .into_iter()
        .map(|mut r| Event {
            pos: r.take(1).unwrap_or_default(),
            kind: r.take(2).unwrap_or_default(),
            server_id: r.take(3).unwrap_or_default(),
            end_pos: r.take(4).unwrap_or_default(),
            info: r.take::<Option<String>, _>(5).flatten().unwrap_or_default(),
        })
        .collect())
}

/// Deletes every binary log file before `file` (`file` itself is kept).
pub fn purge_sql(file: &str) -> String {
    format!("PURGE BINARY LOGS TO {}", literal(file))
}

pub async fn purge(pool: &Pool, file: &str) -> AppResult<()> {
    let mut conn = pool.get_conn().await?;
    file_size(&mut conn, file).await?;
    conn.query_drop(purge_sql(file)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(tls: TlsMode) -> Cli {
        Cli { host: "db.example".into(), port: 3307, socket: None, user: "ann".into(), password: Some("pw".into()), tls, ca: Some("/ca.pem".into()), cert: None, key: None }
    }

    #[test]
    fn flashback_command_line() {
        let a = flashback_args(&cli(TlsMode::VerifyIdentity), "bin.000004", 330, 900);
        assert_eq!(
            a,
            [
                "--no-defaults", "--host=db.example", "--port=3307", "--protocol=TCP", "--user=ann", "--ssl", "--ssl-verify-server-cert",
                "--ssl-ca=/ca.pem", "--read-from-remote-server", "--flashback", "--verbose", "--start-position=330", "--stop-position=900",
                "bin.000004"
            ]
        );
        assert!(!a.iter().any(|x| x.contains("pw")), "the password never goes on the command line");
        let plain = flashback_args(&Cli { socket: Some("/tmp/m.sock".into()), ..cli(TlsMode::Disabled) }, "b.1", 4, 5);
        assert!(plain.contains(&"--socket=/tmp/m.sock".to_string()) && plain.contains(&"--skip-ssl".to_string()));
        assert!(!plain.iter().any(|x| x.starts_with("--host")));
        // Through a tunnel (127.0.0.1) the server name cannot match: CA only.
        let tunnel = flashback_args(&Cli { host: "127.0.0.1".into(), ..cli(TlsMode::VerifyIdentity) }, "b.1", 4, 5);
        assert!(tunnel.contains(&"--skip-ssl-verify-server-cert".to_string()));
    }

    #[test]
    fn quotes_file_name() {
        assert_eq!(events_sql("mysql-bin.000002", None, 1000, 500), "SHOW BINLOG EVENTS IN 'mysql-bin.000002' LIMIT 1000, 500");
        assert_eq!(events_sql("a'b", None, 0, 1), "SHOW BINLOG EVENTS IN 'a''b' LIMIT 0, 1");
        assert_eq!(events_sql("b.1", Some(842), 0, 100), "SHOW BINLOG EVENTS IN 'b.1' FROM 842 LIMIT 0, 100");
        assert_eq!(purge_sql("bin.0'7"), "PURGE BINARY LOGS TO 'bin.0''7'");
    }

    /// Needs a server with log_bin: OTARIA_TEST_BINLOG_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn binlog_on_test_server() {
        let spec = std::env::var("OTARIA_TEST_BINLOG_DB").expect("set OTARIA_TEST_BINLOG_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let pool = Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        );
        let mut conn = pool.get_conn().await.unwrap();
        conn.query_drop(
            "DROP DATABASE IF EXISTS otaria_bl; FLUSH BINARY LOGS; CREATE DATABASE otaria_bl;
             CREATE TABLE otaria_bl.t (id INT PRIMARY KEY, v VARCHAR(10)); INSERT INTO otaria_bl.t VALUES (1, 'a');",
        )
        .await
        .unwrap();
        let st = state(&pool).await.unwrap();
        assert_eq!(st.format, "ROW");
        assert!(!st.gtid.is_empty());
        let last = &st.files.last().unwrap().name;
        let all = events(&pool, last, None, 0, 5000).await.unwrap();
        assert_eq!(all[0].kind, "Format_desc");
        assert!(all.iter().any(|e| e.kind == "Query" && e.info.contains("CREATE TABLE otaria_bl.t")));
        assert!(all.iter().any(|e| e.kind == "Annotate_rows" && e.info.contains("INSERT INTO otaria_bl.t")));
        // Paging: the second page starts where the first ended.
        let page = events(&pool, last, None, 2, 2).await.unwrap();
        assert_eq!((page.len(), page[0].pos), (2, all[2].pos));
        assert_eq!(events(&pool, "nope.000001", None, 0, 10).await.unwrap_err().code, "NOT_FOUND");
        // From a byte position: an event start works, the middle of an event goes back to its start,
        // past the end of the file is refused.
        let from = events(&pool, last, Some(all[3].pos), 0, 2).await.unwrap();
        assert_eq!(from[0].pos, all[3].pos);
        assert_eq!(events(&pool, last, Some(all[3].pos + 1), 0, 2).await.unwrap()[0].pos, all[3].pos);
        assert_eq!(events(&pool, last, Some(st.files.last().unwrap().size + 10), 0, 2).await.unwrap_err().code, "BINLOG_POSITION");

        // Flashback: undo one UPDATE transaction, then replay the script the way the SQL editor splits it.
        async fn at(conn: &mut mysql_async::Conn) -> u64 {
            let mut r: mysql_async::Row = conn.query_first("SHOW MASTER STATUS").await.unwrap().unwrap();
            r.take(1).unwrap()
        }
        let start = at(&mut conn).await;
        conn.query_drop("UPDATE otaria_bl.t SET v = 'b' WHERE id = 1").await.unwrap();
        let stop = at(&mut conn).await;
        let cli = Cli {
            host: p[0].into(), port: p[1].parse().unwrap(), socket: None, user: p[2].into(), password: Some(p[3].into()),
            tls: TlsMode::Required, ca: None, cert: None, key: None,
        };
        let undo = flashback(&pool, &cli, last, start, stop).await.unwrap();
        assert!(undo.contains("BINLOG '") && undo.contains("### UPDATE `otaria_bl`.`t`"), "{undo}");
        for stmt in crate::query::split(&undo) {
            conn.query_drop(&stmt.sql).await.unwrap_or_else(|e| panic!("{e}: {}", stmt.sql));
        }
        let v: Option<String> = conn.query_first("SELECT v FROM otaria_bl.t WHERE id = 1").await.unwrap();
        assert_eq!(v.as_deref(), Some("a"));
        assert_eq!(flashback(&pool, &cli, last, stop, start).await.unwrap_err().code, "INVALID_RANGE");
        let bad = Cli { password: Some("wrong".into()), ..cli };
        assert_eq!(flashback(&pool, &bad, last, start, stop).await.unwrap_err().code, "BINLOG_TOOL");
        conn.query_drop("DROP DATABASE otaria_bl").await.unwrap();

        // Purge: the files before the one named go, that one stays.
        conn.query_drop("FLUSH BINARY LOGS").await.unwrap();
        let names: Vec<String> = state(&pool).await.unwrap().files.into_iter().map(|f| f.name).collect();
        purge(&pool, &names[names.len() - 2]).await.unwrap();
        let left: Vec<String> = state(&pool).await.unwrap().files.into_iter().map(|f| f.name).collect();
        assert_eq!(left, names[names.len() - 2..]);
        assert_eq!(purge(&pool, "nope.000001").await.unwrap_err().code, "NOT_FOUND");

        // A server without binary log says so instead of failing on SHOW BINARY LOGS.
        if let Ok(other) = std::env::var("OTARIA_TEST_DB") {
            let p: Vec<&str> = other.splitn(4, ':').collect();
            let plain = Pool::new(
                mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
            );
            assert_eq!(state(&plain).await.unwrap_err().code, "BINLOG_OFF");
        }
    }
}
