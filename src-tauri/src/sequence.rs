//! MariaDB sequences: current state, CREATE / ALTER SEQUENCE, SETVAL.

use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use serde::{Deserialize, Serialize};

use crate::browse::ident;
use crate::error::{AppError, AppResult};

/// Sequence options. Numbers travel as strings: BIGINT bounds do not fit in a JS number.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SeqOpts {
    pub start: String,
    pub min: String,
    pub max: String,
    pub increment: String,
    pub cache: String,
    pub cycle: bool,
    /// ALTER only: next value to hand out. Empty = unchanged.
    #[serde(default)]
    pub restart: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeqState {
    pub opts: SeqOpts,
    /// First value not yet reserved by any cache (what `SELECT * FROM seq` shows).
    pub next_not_cached: String,
    pub cycle_count: String,
}

/// DDL cannot take parameters: values are parsed as integers and written back from the parsed number,
/// so nothing typed by the user reaches the SQL text as-is.
fn int(field: &'static str, s: &str) -> AppResult<i128> {
    s.trim().parse::<i128>().map_err(|_| AppError { detail: Some(field.into()), ..AppError::new("INVALID_NUMBER", s.to_string()) })
}

pub fn ddl(schema: &str, name: &str, o: &SeqOpts, create: bool) -> AppResult<String> {
    let mut sql = format!(
        "{} SEQUENCE {}.{} START WITH {} MINVALUE {} MAXVALUE {} INCREMENT BY {} CACHE {} {}",
        if create { "CREATE" } else { "ALTER" },
        ident(schema),
        ident(name),
        int("start", &o.start)?,
        int("min", &o.min)?,
        int("max", &o.max)?,
        int("increment", &o.increment)?,
        int("cache", &o.cache)?,
        if o.cycle { "CYCLE" } else { "NOCYCLE" },
    );
    if !create && !o.restart.trim().is_empty() {
        sql += &format!(" RESTART WITH {}", int("restart", &o.restart)?);
    }
    Ok(sql)
}

pub async fn state(pool: &Pool, schema: &str, name: &str) -> AppResult<SeqState> {
    let mut conn = pool.get_conn().await?;
    // CAST: unsigned sequences (11.5+) can exceed i64; read every value as text.
    type Raw = (String, String, String, String, String, String, bool, String);
    let row: Option<Raw> = conn
        .query_first(format!(
            "SELECT CAST(start_value AS CHAR), CAST(minimum_value AS CHAR), CAST(maximum_value AS CHAR),
                    CAST(increment AS CHAR), CAST(cache_size AS CHAR), CAST(next_not_cached_value AS CHAR),
                    cycle_option, CAST(cycle_count AS CHAR) FROM {}.{}",
            ident(schema),
            ident(name)
        ))
        .await?;
    let (start, min, max, increment, cache, next_not_cached, cycle, cycle_count) =
        row.ok_or_else(|| AppError::new("DB", "empty sequence"))?;
    Ok(SeqState { opts: SeqOpts { start, min, max, increment, cache, cycle, restart: String::new() }, next_not_cached, cycle_count })
}

pub async fn apply(pool: &Pool, sql: &str) -> AppResult<()> {
    pool.get_conn().await?.query_drop(sql).await?;
    Ok(())
}

/// Make `value` the next value handed out. Returns None when the server ignored it
/// (SETVAL never moves a sequence backwards: use RESTART for that).
pub async fn setval(pool: &Pool, schema: &str, name: &str, value: &str) -> AppResult<Option<String>> {
    let sql = format!("SELECT CAST(SETVAL({}.{}, {}, 0) AS CHAR)", ident(schema), ident(name), int("value", value)?);
    Ok(pool.get_conn().await?.query_first::<Option<String>, _>(sql).await?.flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> SeqOpts {
        SeqOpts {
            start: "5".into(),
            min: "1".into(),
            max: "9223372036854775806".into(),
            increment: "2".into(),
            cache: "1000".into(),
            cycle: false,
            restart: " ".into(),
        }
    }

    #[test]
    fn builds_sequence_ddl() {
        assert_eq!(
            ddl("s", "q`1", &opts(), true).unwrap(),
            "CREATE SEQUENCE `s`.`q``1` START WITH 5 MINVALUE 1 MAXVALUE 9223372036854775806 INCREMENT BY 2 CACHE 1000 NOCYCLE"
        );
        let o = SeqOpts { cycle: true, restart: "42".into(), increment: "-1".into(), ..opts() };
        assert!(ddl("s", "q", &o, false).unwrap().ends_with("INCREMENT BY -1 CACHE 1000 CYCLE RESTART WITH 42"));
        // RESTART is ignored on CREATE.
        assert!(!ddl("s", "q", &o, true).unwrap().contains("RESTART"));
    }

    /// OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn sequences_on_test_server() {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let pool = Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        );
        pool.get_conn().await.unwrap().query_drop("CREATE DATABASE IF NOT EXISTS otaria_seq").await.unwrap();
        apply(&pool, "DROP SEQUENCE IF EXISTS otaria_seq.q").await.unwrap();
        apply(&pool, &ddl("otaria_seq", "q", &opts(), true).unwrap()).await.unwrap();
        let s = state(&pool, "otaria_seq", "q").await.unwrap();
        assert_eq!((s.opts.start.as_str(), s.opts.max.as_str(), s.next_not_cached.as_str()), ("5", "9223372036854775806", "5"));

        assert_eq!(setval(&pool, "otaria_seq", "q", "100").await.unwrap().as_deref(), Some("100"));
        // Backwards: ignored by the server.
        assert_eq!(setval(&pool, "otaria_seq", "q", "3").await.unwrap(), None);
        let next: Option<String> = pool.get_conn().await.unwrap().query_first("SELECT CAST(NEXTVAL(otaria_seq.q) AS CHAR)").await.unwrap();
        assert_eq!(next.as_deref(), Some("100"));

        let alt = SeqOpts { increment: "10".into(), restart: "7".into(), ..s.opts };
        apply(&pool, &ddl("otaria_seq", "q", &alt, false).unwrap()).await.unwrap();
        let next: Option<String> = pool.get_conn().await.unwrap().query_first("SELECT CAST(NEXTVAL(otaria_seq.q) AS CHAR)").await.unwrap();
        assert_eq!(next.as_deref(), Some("7"));
        apply(&pool, "DROP DATABASE otaria_seq").await.unwrap();
    }

    #[test]
    fn rejects_non_numbers() {
        let bad = SeqOpts { cache: "1; DROP TABLE t".into(), ..opts() };
        let e = ddl("s", "q", &bad, true).unwrap_err();
        assert_eq!((e.code, e.detail.as_deref()), ("INVALID_NUMBER", Some("cache")));
    }
}
