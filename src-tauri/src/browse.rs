//! Read-only browsing: object tree, table structure, DDL and data pages.

use mysql_async::prelude::Queryable;
use mysql_async::{Pool, Row, Value};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// Quote an identifier for MariaDB: `name`, with backticks doubled.
pub fn ident(s: &str) -> String {
    format!("`{}`", s.replace('`', "``"))
}

pub async fn schemas(pool: &Pool) -> AppResult<Vec<String>> {
    let mut conn = pool.get_conn().await?;
    Ok(conn.query("SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME").await?)
}

#[derive(Debug, Serialize)]
pub struct DbObject {
    pub name: String,
    /// table | view | sequence | procedure | function | trigger | event
    pub kind: String,
    /// MariaDB system-versioned table (used by phase 3b).
    pub versioned: bool,
}

/// Every object of a schema, in one round trip.
pub async fn objects(pool: &Pool, schema: &str) -> AppResult<Vec<DbObject>> {
    let sql = "
        SELECT TABLE_NAME, CASE TABLE_TYPE WHEN 'VIEW' THEN 'view' WHEN 'SYSTEM VIEW' THEN 'view'
               WHEN 'SEQUENCE' THEN 'sequence' ELSE 'table' END, TABLE_TYPE = 'SYSTEM VERSIONED'
          FROM information_schema.TABLES WHERE TABLE_SCHEMA = ?
        UNION ALL SELECT ROUTINE_NAME, LOWER(ROUTINE_TYPE), 0
          FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ? AND ROUTINE_TYPE IN ('PROCEDURE', 'FUNCTION')
        UNION ALL SELECT TRIGGER_NAME, 'trigger', 0 FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ?
        UNION ALL SELECT EVENT_NAME, 'event', 0 FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ?
        ORDER BY 1";
    let mut conn = pool.get_conn().await?;
    let rows: Vec<(String, String, bool)> = conn.exec(sql, (schema, schema, schema, schema)).await?;
    Ok(rows.into_iter().map(|(name, kind, versioned)| DbObject { name, kind, versioned }).collect())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub column_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub extra: String,
    pub comment: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableInfo {
    pub columns: Vec<Column>,
    /// Primary key columns in key order; empty = the grid is read-only.
    pub primary_key: Vec<String>,
}

pub async fn table_info(pool: &Pool, schema: &str, table: &str) -> AppResult<TableInfo> {
    let mut conn = pool.get_conn().await?;
    let cols: Vec<Row> = conn
        .exec(
            "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, IS_NULLABLE = 'YES', COLUMN_DEFAULT, EXTRA, COLUMN_COMMENT
               FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
            (schema, table),
        )
        .await?;
    let columns = cols
        .into_iter()
        .map(|mut r| Column {
            name: r.take(0).unwrap_or_default(),
            data_type: r.take(1).unwrap_or_default(),
            column_type: r.take(2).unwrap_or_default(),
            nullable: r.take(3).unwrap_or_default(),
            default: r.take(4).unwrap_or_default(),
            extra: r.take(5).unwrap_or_default(),
            comment: r.take(6).unwrap_or_default(),
        })
        .collect();
    let primary_key = conn
        .exec(
            "SELECT COLUMN_NAME FROM information_schema.STATISTICS
              WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND INDEX_NAME = 'PRIMARY' ORDER BY SEQ_IN_INDEX",
            (schema, table),
        )
        .await?;
    Ok(TableInfo { columns, primary_key })
}

/// `SHOW CREATE …` for any object kind of the tree.
pub async fn ddl(pool: &Pool, schema: &str, name: &str, kind: &str) -> AppResult<String> {
    // Column holding the statement differs per kind.
    let (keyword, col) = match kind {
        "table" => ("TABLE", 1),
        "view" => ("VIEW", 1),
        "sequence" => ("SEQUENCE", 1),
        "procedure" => ("PROCEDURE", 2),
        "function" => ("FUNCTION", 2),
        "trigger" => ("TRIGGER", 2),
        "event" => ("EVENT", 3),
        _ => return Err(AppError::new("UNKNOWN", format!("kind {kind}"))),
    };
    let mut conn = pool.get_conn().await?;
    let row: Option<Row> = conn.query_first(format!("SHOW CREATE {keyword} {}.{}", ident(schema), ident(name))).await?;
    // Routines return NULL here when the user lacks the privilege to see the body.
    let text: Option<String> = row.and_then(|mut r| r.take::<Option<String>, _>(col).flatten());
    text.ok_or_else(|| AppError::new("NO_DDL", name.to_string()))
}

#[derive(Debug, Deserialize)]
pub struct Sort {
    pub column: String,
    pub desc: bool,
}

#[derive(Debug, Serialize)]
pub struct Rows {
    pub columns: Vec<String>,
    /// Text values as sent by the server; NULL = None. Non-UTF-8 bytes come as 0x… hex.
    pub rows: Vec<Vec<Option<String>>>,
}

/// Session variable holding the "as of" timestamp of a versioned table (set by `data`, never inlined).
pub const AS_OF_VAR: &str = "@otaria_as_of";

/// `as_of`: read a system-versioned table as it was at the timestamp held in `AS_OF_VAR`.
pub fn data_sql(schema: &str, table: &str, filter: Option<&str>, order: &[Sort], offset: u64, limit: u64, as_of: bool) -> String {
    let mut sql = format!("SELECT * FROM {}.{}", ident(schema), ident(table));
    if as_of {
        sql += &format!(" FOR SYSTEM_TIME AS OF TIMESTAMP {AS_OF_VAR}");
    }
    // The filter is the user's own SQL, run with their own privileges, like any query they type.
    if let Some(f) = filter.map(str::trim).filter(|f| !f.is_empty()) {
        sql += &format!(" WHERE ({f}\n)");
    }
    if !order.is_empty() {
        let cols: Vec<String> =
            order.iter().map(|s| format!("{}{}", ident(&s.column), if s.desc { " DESC" } else { "" })).collect();
        sql += &format!(" ORDER BY {}", cols.join(", "));
    }
    // ponytail: LIMIT/OFFSET, slow on very deep pages of huge tables; switch to keyset on the PK if it matters.
    sql + &format!(" LIMIT {limit} OFFSET {offset}")
}

pub fn cell(v: Value) -> Option<String> {
    match v {
        Value::NULL => None,
        Value::Bytes(b) => Some(String::from_utf8(b).unwrap_or_else(|e| {
            e.as_bytes().iter().fold(String::from("0x"), |s, b| s + &format!("{b:02X}"))
        })),
        // The text protocol only sends bytes; keep a readable fallback anyway.
        other => Some(other.as_sql(true).trim_matches('\'').to_string()),
    }
}

// ponytail: whole cells are loaded (no BLOB truncation), heavy for big BLOB columns × 1000 rows.
/// `vars`: user values bound as session variables first (prepared), so the query itself stays on the
/// text protocol. With vars, timestamps are in UTC (`time_zone = '+00:00'`); the pool resets the
/// connection when it is returned, so this never leaks into other queries.
pub async fn data(pool: &Pool, sql: &str, vars: &[(&str, Option<String>)]) -> AppResult<Rows> {
    let mut conn = pool.get_conn().await?;
    if !vars.is_empty() {
        let sets: Vec<String> = vars.iter().map(|(name, _)| format!("{name} = ?")).collect();
        let params: Vec<Value> = vars.iter().map(|(_, v)| Value::from(v.clone())).collect();
        conn.exec_drop(format!("SET time_zone = '+00:00', {}", sets.join(", ")), params).await?;
    }
    let mut result = conn.query_iter(sql).await?;
    let columns = result.columns_ref().iter().map(|c| c.name_str().into_owned()).collect();
    let rows = result.map(|row: Row| row.unwrap().into_iter().map(cell).collect()).await?;
    drop(result);
    Ok(Rows { columns, rows })
}

/// Names of the ROW START / ROW END columns, read from `SHOW CREATE TABLE`.
/// Implicit ones (`WITH SYSTEM VERSIONING` alone) are invisible and named ROW_START / ROW_END.
pub fn period_columns(ddl: &str) -> (String, String) {
    let parse = || {
        let at = ddl.find("PERIOD FOR SYSTEM_TIME (")?;
        let mut names = ddl[at..].split('`').skip(1).step_by(2).map(|n| n.replace("``", "`"));
        Some((names.next()?, names.next()?))
    };
    // ponytail: a column name containing a backtick breaks the naive split; such names are rare.
    parse().unwrap_or_else(|| ("ROW_START".into(), "ROW_END".into()))
}

#[derive(Debug, Serialize)]
pub struct History {
    pub rows: Rows,
    /// Period columns in `rows` (UTC timestamps).
    pub start: String,
    pub end: String,
}

/// Every version of one row of a system-versioned table, oldest first, with its validity period (UTC).
pub async fn row_history(pool: &Pool, schema: &str, table: &str, key: &[(String, Option<String>)]) -> AppResult<History> {
    if key.is_empty() {
        return Err(AppError::new("NO_PRIMARY_KEY", table.to_string()));
    }
    let (start, end) = period_columns(&ddl(pool, schema, table, "table").await?);
    // Implicit period columns are invisible: name them explicitly; explicit ones are already in `*`.
    let extra = if start == "ROW_START" { format!(", {}, {}", ident(&start), ident(&end)) } else { String::new() };
    let names: Vec<String> = (0..key.len()).map(|i| format!("@otaria_k{i}")).collect();
    let cond: Vec<String> = key.iter().zip(&names).map(|((c, _), n)| format!("{} <=> {n}", ident(c))).collect();
    let sql = format!(
        "SELECT *{extra} FROM {}.{} FOR SYSTEM_TIME ALL WHERE {} ORDER BY {}",
        ident(schema),
        ident(table),
        cond.join(" AND "),
        ident(&end)
    );
    let vars: Vec<(&str, Option<String>)> = names.iter().map(String::as_str).zip(key.iter().map(|(_, v)| v.clone())).collect();
    Ok(History { rows: data(pool, &sql, &vars).await?, start, end })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_identifiers() {
        assert_eq!(ident("a`b"), "`a``b`");
    }

    #[test]
    fn builds_data_query() {
        let order = [Sort { column: "name".into(), desc: true }, Sort { column: "id".into(), desc: false }];
        assert_eq!(
            data_sql("s", "t", Some(" id > 3 "), &order, 1000, 500, false),
            "SELECT * FROM `s`.`t` WHERE (id > 3\n) ORDER BY `name` DESC, `id` LIMIT 500 OFFSET 1000"
        );
        assert_eq!(data_sql("s", "t", Some("  "), &[], 0, 10, false), "SELECT * FROM `s`.`t` LIMIT 10 OFFSET 0");
        assert_eq!(
            data_sql("s", "t", None, &[], 0, 10, true),
            "SELECT * FROM `s`.`t` FOR SYSTEM_TIME AS OF TIMESTAMP @otaria_as_of LIMIT 10 OFFSET 0"
        );
    }

    #[test]
    fn finds_period_columns() {
        let explicit = "CREATE TABLE `ve` (\n  `rs` timestamp(6) GENERATED ALWAYS AS ROW START,\n  `re` timestamp(6) GENERATED ALWAYS AS ROW END,\n  PERIOD FOR SYSTEM_TIME (`rs`, `re`)\n) WITH SYSTEM VERSIONING";
        assert_eq!(period_columns(explicit), ("rs".into(), "re".into()));
        let implicit = "CREATE TABLE `vi` (\n  `id` int(11) NOT NULL\n) WITH SYSTEM VERSIONING";
        assert_eq!(period_columns(implicit), ("ROW_START".into(), "ROW_END".into()));
    }

    #[test]
    fn non_utf8_cells_become_hex() {
        assert_eq!(cell(Value::Bytes(vec![0xff, 0x01])), Some("0xFF01".into()));
        assert_eq!(cell(Value::NULL), None);
    }

    /// OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn versioned_tables_on_test_server() {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let pool = Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        );
        let mut conn = pool.get_conn().await.unwrap();
        conn.query_drop(
            "CREATE DATABASE IF NOT EXISTS otaria_ver; USE otaria_ver; DROP TABLE IF EXISTS vi, ve;
             CREATE TABLE vi (id INT PRIMARY KEY, v VARCHAR(10)) WITH SYSTEM VERSIONING;
             CREATE TABLE ve (id INT PRIMARY KEY, v INT, rs TIMESTAMP(6) GENERATED ALWAYS AS ROW START,
               re TIMESTAMP(6) GENERATED ALWAYS AS ROW END, PERIOD FOR SYSTEM_TIME(rs, re)) WITH SYSTEM VERSIONING;
             INSERT INTO vi VALUES (1, 'a'); INSERT INTO ve (id, v) VALUES (1, 10);",
        )
        .await
        .unwrap();
        // Remember "now" in UTC, then change the rows.
        let before: String = conn.query_first("SELECT CAST(UTC_TIMESTAMP(6) AS CHAR)").await.unwrap().unwrap();
        conn.query_drop("DO SLEEP(0.01); UPDATE vi SET v = 'b' WHERE id = 1; UPDATE ve SET v = 20 WHERE id = 1").await.unwrap();
        drop(conn);

        let past = data(&pool, &data_sql("otaria_ver", "vi", None, &[], 0, 10, true), &[(AS_OF_VAR, Some(before))]).await.unwrap();
        assert_eq!(past.rows, [[Some("1".to_string()), Some("a".to_string())]]);

        let key = [("id".to_string(), Some("1".to_string()))];
        let h = row_history(&pool, "otaria_ver", "vi", &key).await.unwrap().rows;
        assert_eq!(h.columns, ["id", "v", "ROW_START", "ROW_END"]);
        assert_eq!(h.rows.iter().map(|r| r[1].clone().unwrap()).collect::<Vec<_>>(), ["a", "b"]);
        let h = row_history(&pool, "otaria_ver", "ve", &key).await.unwrap().rows;
        assert_eq!(h.columns, ["id", "v", "rs", "re"]);
        assert_eq!(h.rows.len(), 2);
        pool.get_conn().await.unwrap().query_drop("DROP DATABASE otaria_ver").await.unwrap();
    }
}
