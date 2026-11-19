//! Server administration: process list (KILL, EXPLAIN FOR CONNECTION), global variables (SET GLOBAL),
//! global status for the dashboard, storage engines. Users and roles: `users.rs`.

use std::collections::HashMap;

use mysql_async::prelude::Queryable;
use mysql_async::{Pool, Value};
use serde::Serialize;

use crate::error::{AppError, AppResult};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Process {
    pub id: u64,
    pub user: String,
    pub host: String,
    pub db: Option<String>,
    pub command: String,
    pub time_ms: f64,
    pub state: Option<String>,
    pub info: Option<String>,
    /// 0–100, MariaDB only (ALTER TABLE, LOAD DATA…); 0 elsewhere.
    pub progress: f64,
    /// One of this app's own connections: the one listing or an editor tab's.
    pub own: bool,
}

/// `own`: connection ids the app holds (editor tabs); the listing connection is added here.
pub async fn processes(pool: &Pool, mariadb: bool, own: &[u32]) -> AppResult<Vec<Process>> {
    // TIME_MS and PROGRESS are MariaDB columns. `+ 0e0` turns DECIMAL into DOUBLE.
    let sql = if mariadb {
        "SELECT ID, USER, HOST, DB, COMMAND, TIME_MS + 0e0, STATE, INFO, PROGRESS + 0e0, ID = CONNECTION_ID() FROM information_schema.PROCESSLIST ORDER BY ID"
    } else {
        "SELECT ID, USER, HOST, DB, COMMAND, TIME * 1e3, STATE, INFO, 0e0, ID = CONNECTION_ID() FROM information_schema.PROCESSLIST ORDER BY ID"
    };
    Ok(pool
        .get_conn()
        .await?
        .query_map(sql, |(id, user, host, db, command, time_ms, state, info, progress, me): (u64, _, _, _, _, _, _, _, _, bool)| {
            Process { id, user, host, db, command, time_ms, state, info, progress, own: me || own.iter().any(|&o| o as u64 == id) }
        })
        .await?)
}

/// The id is a number, so formatting it is safe; KILL takes no placeholder.
pub fn kill_sql(id: u64, connection: bool) -> String {
    format!("KILL {} {id}", if connection { "CONNECTION" } else { "QUERY" })
}

pub async fn kill(pool: &Pool, id: u64, connection: bool) -> AppResult<()> {
    pool.get_conn().await?.query_drop(kill_sql(id, connection)).await?;
    Ok(())
}

/// Plan of the statement another connection is running (MariaDB ≥ 10.9, MySQL ≥ 5.7).
pub async fn explain(pool: &Pool, id: u64) -> AppResult<Option<String>> {
    Ok(pool.get_conn().await?.query_first(format!("EXPLAIN FORMAT=JSON FOR CONNECTION {id}")).await?)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    pub value: Option<String>,
    /// The rest is MariaDB only (information_schema.SYSTEM_VARIABLES), None on MySQL.
    pub default: Option<String>,
    /// INT, BIGINT UNSIGNED, DOUBLE, BOOLEAN, ENUM, SET, FLAGSET, VARCHAR…
    pub kind: Option<String>,
    /// COMPILE-TIME | CONFIG | COMMAND-LINE | AUTO | SQL
    pub origin: Option<String>,
    pub read_only: bool,
    pub comment: Option<String>,
    pub choices: Option<String>,
}

pub async fn variables(pool: &Pool, mariadb: bool) -> AppResult<Vec<Variable>> {
    let mut conn = pool.get_conn().await?;
    if !mariadb {
        // ponytail: no default/type on MySQL (performance_schema.variables_info has no default); add if MySQL users ask.
        return Ok(conn
            .query_map("SHOW GLOBAL VARIABLES", |(name, value)| Variable {
                name,
                value: Some(value),
                default: None,
                kind: None,
                origin: None,
                read_only: false,
                comment: None,
                choices: None,
            })
            .await?);
    }
    Ok(conn
        .query_map(
            "SELECT LOWER(VARIABLE_NAME), GLOBAL_VALUE, DEFAULT_VALUE, VARIABLE_TYPE, GLOBAL_VALUE_ORIGIN, READ_ONLY = 'YES', VARIABLE_COMMENT, ENUM_VALUE_LIST
             FROM information_schema.SYSTEM_VARIABLES WHERE VARIABLE_SCOPE <> 'SESSION ONLY' ORDER BY 1",
            |(name, value, default, kind, origin, read_only, comment, choices)| Variable {
                name,
                value,
                default,
                kind: Some(kind),
                origin: Some(origin),
                read_only,
                comment,
                choices,
            },
        )
        .await?)
}

/// Variable names are plain words; they go into the statement unquoted.
fn check_name(name: &str) -> AppResult<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(AppError::new("INVALID_NAME", name.to_string()));
    }
    Ok(())
}

/// The server refuses a numeric variable given as a string ('200'), so bind the value with its type.
/// `kind` None (MySQL): a number if it parses as one, else a string.
fn typed(kind: Option<&str>, v: &str) -> AppResult<Value> {
    let bad = || AppError::new("VARIABLE_VALUE", v.to_string());
    let n = v.trim();
    Ok(match kind {
        Some(k) if k.contains("INT") && k.contains("UNSIGNED") => Value::UInt(n.parse().map_err(|_| bad())?),
        Some(k) if k.contains("INT") => Value::Int(n.parse().map_err(|_| bad())?),
        Some("DOUBLE") => Value::Double(n.parse().map_err(|_| bad())?),
        Some(_) => Value::from(v),
        None => n.parse::<i64>().map(Value::Int).or_else(|_| n.parse::<f64>().map(Value::Double)).unwrap_or_else(|_| Value::from(v)),
    })
}

/// Shown in the preview. `value` None = back to the default.
pub fn set_sql(name: &str, kind: Option<&str>, value: Option<&str>) -> AppResult<String> {
    check_name(name)?;
    let v = match value {
        Some(v) => typed(kind, v)?.as_sql(false),
        None => "DEFAULT".into(),
    };
    Ok(format!("SET GLOBAL {name} = {v}"))
}

/// The type is read again from the server rather than trusted from the UI.
pub async fn set(pool: &Pool, mariadb: bool, name: &str, value: Option<&str>) -> AppResult<()> {
    check_name(name)?;
    let mut conn = pool.get_conn().await?;
    let kind: Option<String> = if mariadb {
        let k: Option<String> = conn.exec_first("SELECT VARIABLE_TYPE FROM information_schema.SYSTEM_VARIABLES WHERE VARIABLE_NAME = ?", (name,)).await?;
        Some(k.ok_or_else(|| AppError::new("NOT_FOUND", name.to_string()))?)
    } else {
        None
    };
    match value {
        Some(v) => conn.exec_drop(format!("SET GLOBAL {name} = ?"), (typed(kind.as_deref(), v)?,)).await?,
        None => conn.query_drop(format!("SET GLOBAL {name} = DEFAULT")).await?,
    }
    Ok(())
}

/// SHOW GLOBAL STATUS plus max_connections; the UI computes rates from two samples.
pub async fn status(pool: &Pool) -> AppResult<HashMap<String, String>> {
    let mut conn = pool.get_conn().await?;
    let mut m: HashMap<String, String> = conn.query_map("SHOW GLOBAL STATUS", |(k, v): (String, Option<String>)| (k, v.unwrap_or_default())).await?.into_iter().collect();
    let max: Option<String> = conn.query_first("SELECT @@max_connections").await?;
    m.insert("max_connections".into(), max.unwrap_or_default());
    Ok(m)
}

/// Engines from `SHOW ENGINES` (also MySQL). Engines shipped as plugins (MyRocks, Spider, CONNECT,
/// ColumnStore) only appear once installed: the Plugins tab installs them.
#[derive(Debug, Serialize)]
pub struct Engine {
    pub name: String,
    /// YES | NO | DEFAULT | DISABLED
    pub support: String,
    pub comment: String,
    pub transactions: bool,
    pub xa: bool,
    pub savepoints: bool,
}

pub async fn engines(pool: &Pool) -> AppResult<Vec<Engine>> {
    let rows: Vec<mysql_async::Row> = pool.get_conn().await?.query("SHOW ENGINES").await?;
    let yes = |v: Option<String>| v.as_deref() == Some("YES");
    Ok(rows
        .into_iter()
        .map(|mut r| Engine {
            name: r.take(0).unwrap_or_default(),
            support: r.take(1).unwrap_or_default(),
            comment: r.take::<Option<String>, _>(2).flatten().unwrap_or_default(),
            transactions: yes(r.take(3).flatten()),
            xa: yes(r.take(4).flatten()),
            savepoints: yes(r.take(5).flatten()),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_sql_types_values_and_rejects_bad_names() {
        assert_eq!(set_sql("max_connections", Some("INT UNSIGNED"), Some(" 200 ")).unwrap(), "SET GLOBAL max_connections = 200");
        assert_eq!(set_sql("long_query_time", Some("DOUBLE"), Some("2.5")).unwrap(), "SET GLOBAL long_query_time = 2.5");
        assert_eq!(set_sql("init_connect", Some("VARCHAR"), Some("SET x='1'")).unwrap(), "SET GLOBAL init_connect = 'SET x=\\'1\\''");
        assert_eq!(set_sql("general_log", Some("BOOLEAN"), Some("ON")).unwrap(), "SET GLOBAL general_log = 'ON'");
        assert_eq!(set_sql("max_connections", Some("INT UNSIGNED"), None).unwrap(), "SET GLOBAL max_connections = DEFAULT");
        // MySQL (no type): guess.
        assert_eq!(set_sql("x", None, Some("-3")).unwrap(), "SET GLOBAL x = -3");
        assert_eq!(set_sql("x", None, Some("OFF")).unwrap(), "SET GLOBAL x = 'OFF'");
        assert_eq!(set_sql("max_connections", Some("INT UNSIGNED"), Some("-1")).unwrap_err().code, "VARIABLE_VALUE");
        assert_eq!(set_sql("long_query_time", Some("DOUBLE"), Some("abc")).unwrap_err().code, "VARIABLE_VALUE");
        for bad in ["", "a b", "x=1;DROP", "a`b", "x.y"] {
            assert_eq!(set_sql(bad, None, Some("1")).unwrap_err().code, "INVALID_NAME", "{bad}");
        }
        assert_eq!(kill_sql(42, false), "KILL QUERY 42");
        assert_eq!(kill_sql(42, true), "KILL CONNECTION 42");
    }

    /// OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn admin_on_test_server() {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let pool = Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        );

        // A sleeping query on its own connection: listed, explainable, killable.
        let mut busy = pool.get_conn().await.unwrap();
        let busy_id = busy.id() as u64;
        let sleeper = tokio::spawn(async move { busy.query_drop("SELECT SLEEP(30) FROM information_schema.TABLES LIMIT 1").await });
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let list = processes(&pool, true, &[busy_id as u32]).await.unwrap();
        let me = list.iter().find(|x| x.id == busy_id).expect("busy connection listed");
        assert!(me.own && list.iter().filter(|x| x.own).count() == 2, "{list:?}");
        assert!(me.info.as_deref().unwrap_or("").contains("SLEEP(30)"), "{me:?}");
        let plan = explain(&pool, busy_id).await.unwrap().expect("plan");
        assert!(plan.contains("query_block"), "{plan}");
        kill(&pool, busy_id, false).await.unwrap();
        // Interrupted now (ER_QUERY_INTERRUPTED) instead of after 30 s.
        let res = tokio::time::timeout(std::time::Duration::from_secs(5), sleeper).await.expect("killed").unwrap();
        assert!(matches!(res, Err(mysql_async::Error::Server(ref e)) if e.code == 1317), "{res:?}");
        assert!(explain(&pool, 999_999).await.is_err());

        // Variables: typed SET GLOBAL, then back to default.
        let vars = variables(&pool, true).await.unwrap();
        let mc = vars.iter().find(|v| v.name == "max_connections").expect("max_connections");
        assert_eq!(mc.kind.as_deref(), Some("BIGINT UNSIGNED"));
        assert!(vars.iter().all(|v| v.name != "pseudo_thread_id"), "session-only variables hidden");
        set(&pool, true, "max_connections", Some("170")).await.unwrap();
        let mut conn = pool.get_conn().await.unwrap();
        assert_eq!(conn.query_first::<u64, _>("SELECT @@global.max_connections").await.unwrap(), Some(170));
        set(&pool, true, "max_connections", None).await.unwrap();
        assert_eq!(set(&pool, true, "no_such_var", Some("1")).await.unwrap_err().code, "NOT_FOUND");
        assert!(set(&pool, true, "version", Some("x")).await.is_err(), "read-only variable refused by the server");

        let st = status(&pool).await.unwrap();
        assert!(st.contains_key("Questions") && st.contains_key("Innodb_buffer_pool_read_requests"));
        assert!(st["max_connections"].parse::<u64>().unwrap() > 0);

        let e = engines(&pool).await.unwrap();
        assert!(e.iter().any(|e| e.name == "InnoDB" && e.support == "DEFAULT" && e.transactions && e.savepoints));
    }
}
