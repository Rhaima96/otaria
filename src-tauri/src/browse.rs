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

pub fn data_sql(schema: &str, table: &str, filter: Option<&str>, order: &[Sort], offset: u64, limit: u64) -> String {
    let mut sql = format!("SELECT * FROM {}.{}", ident(schema), ident(table));
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
pub async fn data(pool: &Pool, sql: &str) -> AppResult<Rows> {
    let mut conn = pool.get_conn().await?;
    let mut result = conn.query_iter(sql).await?;
    let columns = result.columns_ref().iter().map(|c| c.name_str().into_owned()).collect();
    let rows = result.map(|row: Row| row.unwrap().into_iter().map(cell).collect()).await?;
    drop(result);
    Ok(Rows { columns, rows })
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
            data_sql("s", "t", Some(" id > 3 "), &order, 1000, 500),
            "SELECT * FROM `s`.`t` WHERE (id > 3\n) ORDER BY `name` DESC, `id` LIMIT 500 OFFSET 1000"
        );
        assert_eq!(data_sql("s", "t", Some("  "), &[], 0, 10), "SELECT * FROM `s`.`t` LIMIT 10 OFFSET 0");
    }

    #[test]
    fn non_utf8_cells_become_hex() {
        assert_eq!(cell(Value::Bytes(vec![0xff, 0x01])), Some("0xFF01".into()));
        assert_eq!(cell(Value::NULL), None);
    }
}
