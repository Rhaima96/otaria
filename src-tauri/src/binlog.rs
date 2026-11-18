//! Binary log reader (MariaDB only): files from `SHOW BINARY LOGS`, events from `SHOW BINLOG EVENTS`,
//! read one page at a time. Read-only.
// ponytail: no flashback (undo SQL) here: it needs the mariadb-binlog binary with the password,
// tunnel port and TLS files; a separate lot if asked. No PURGE either: the SQL editor does it.

use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use serde::Serialize;

use crate::designer::literal;
use crate::error::{AppError, AppResult};

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
/// then quoted. `offset` counts events, not bytes (a byte position must be an event start).
pub fn events_sql(file: &str, offset: u64, limit: u64) -> String {
    format!("SHOW BINLOG EVENTS IN {} LIMIT {offset}, {limit}", literal(file))
}

pub async fn events(pool: &Pool, file: &str, offset: u64, limit: u64) -> AppResult<Vec<Event>> {
    let mut conn = pool.get_conn().await?;
    let names: Vec<String> = conn.query_map("SHOW BINARY LOGS", |r: mysql_async::Row| r.get::<String, _>(0).unwrap_or_default()).await?;
    if !names.iter().any(|n| n == file) {
        return Err(AppError::new("NOT_FOUND", file.to_string()));
    }
    let rows: Vec<mysql_async::Row> = conn.query(events_sql(file, offset, limit.clamp(1, 5000))).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_file_name() {
        assert_eq!(events_sql("mysql-bin.000002", 1000, 500), "SHOW BINLOG EVENTS IN 'mysql-bin.000002' LIMIT 1000, 500");
        assert_eq!(events_sql("a'b", 0, 1), "SHOW BINLOG EVENTS IN 'a''b' LIMIT 0, 1");
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
        let all = events(&pool, last, 0, 5000).await.unwrap();
        assert_eq!(all[0].kind, "Format_desc");
        assert!(all.iter().any(|e| e.kind == "Query" && e.info.contains("CREATE TABLE otaria_bl.t")));
        assert!(all.iter().any(|e| e.kind == "Annotate_rows" && e.info.contains("INSERT INTO otaria_bl.t")));
        // Paging: the second page starts where the first ended.
        let page = events(&pool, last, 2, 2).await.unwrap();
        assert_eq!((page.len(), page[0].pos), (2, all[2].pos));
        assert_eq!(events(&pool, "nope.000001", 0, 10).await.unwrap_err().code, "NOT_FOUND");
        conn.query_drop("DROP DATABASE otaria_bl").await.unwrap();

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
