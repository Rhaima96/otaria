//! SQL editor: statement splitting (with DELIMITER), execution on a dedicated connection, KILL QUERY.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Pool};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::browse::{cell, ident};
use crate::error::{AppError, AppResult};

/// One editor tab = one server connection, so `USE`, session variables and temporary tables persist
/// between runs, and `KILL QUERY` can target it by id.
pub struct Editor {
    pub conn_id: u32,
    pub conn: Arc<Mutex<Conn>>,
}

pub type Editors = std::sync::Mutex<HashMap<String, Editor>>;

/// Open a connection for an editor tab, optionally on a default schema.
pub async fn open_editor(pool: &Pool, schema: Option<&str>) -> AppResult<Editor> {
    let mut conn = pool.get_conn().await?;
    if let Some(s) = schema.filter(|s| !s.is_empty()) {
        conn.query_drop(format!("USE {}", ident(s))).await?;
    }
    Ok(Editor { conn_id: conn.id(), conn: Arc::new(Mutex::new(conn)) })
}

/// A statement and its byte range in the source text.
#[derive(Debug, PartialEq)]
pub struct Stmt {
    pub sql: String,
    pub start: usize,
    pub end: usize,
}

/// Split a script into statements. Honours quotes, backticks, `--`/`#`/`/* */` comments and the
/// client-side `DELIMITER` command (needed for routine bodies). Statements come without their delimiter.
pub fn split(src: &str) -> Vec<Stmt> {
    let (mut out, tail, _) = split_from(src, ";");
    push_stmt(src, &mut out, tail, src.len());
    out
}

fn push_stmt(src: &str, out: &mut Vec<Stmt>, s: usize, e: usize) {
    let text = &src[s..e];
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        let lead = text.len() - text.trim_start().len();
        out.push(Stmt { sql: trimmed.to_string(), start: s + lead, end: s + lead + trimmed.len() });
    }
}

/// Streaming form of `split`: only statements closed by their delimiter, plus where the unterminated
/// tail starts and the delimiter in force there (to resume on the next chunk of a file).
pub fn split_from(src: &str, delim: &str) -> (Vec<Stmt>, usize, String) {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut delim = delim.to_string();
    let (mut i, mut start) = (0, 0);
    // Nothing but whitespace and comments since `start`: where DELIMITER may appear.
    let mut blank = true;
    while i < b.len() {
        // DELIMITER is only recognised at the start of a statement (ignoring whitespace).
        if blank && src[i..].len() >= 10 && src[i..i + 10].eq_ignore_ascii_case("delimiter ") {
            let line_end = src[i..].find('\n').map_or(b.len(), |n| i + n);
            let d = src[i + 10..line_end].trim();
            if !d.is_empty() {
                delim = d.to_string();
            }
            i = line_end;
            start = i;
            continue;
        }
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                blank = false;
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' && q != b'`' {
                        i += 2;
                        continue;
                    }
                    if b[i] == q {
                        // Doubled quote = escaped quote.
                        if b.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'#' => i = src[i..].find('\n').map_or(b.len(), |n| i + n),
            b'-' if src[i..].starts_with("--") && b.get(i + 2).is_none_or(|c| c.is_ascii_whitespace()) => {
                i = src[i..].find('\n').map_or(b.len(), |n| i + n)
            }
            b'/' if src[i..].starts_with("/*") => i = src[i + 2..].find("*/").map_or(b.len(), |n| i + n + 4),
            _ if src[i..].starts_with(delim.as_str()) => {
                push_stmt(src, &mut out, start, i);
                i += delim.len();
                start = i;
                blank = true;
            }
            c => {
                blank &= c.is_ascii_whitespace();
                i += 1;
            }
        }
    }
    (out, start.min(b.len()), delim)
}

/// Destructive statements that need an explicit confirmation (PLAN §4).
// ponytail: keyword check, a WHERE inside a subquery or string fools it; use a real parser if it matters.
pub fn risky(sql: &str) -> bool {
    let up = sql.to_ascii_uppercase();
    let words: Vec<&str> = up.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').filter(|w| !w.is_empty()).collect();
    match words.first().copied() {
        Some("DROP" | "TRUNCATE") => true,
        Some("DELETE" | "UPDATE") => !words.contains(&"WHERE"),
        _ => false,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StmtResult {
    pub sql: String,
    /// Empty when the statement returns no result set (INSERT, SET…).
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// Rows the server sent; more than `rows.len()` when the display cap was hit.
    pub total_rows: u64,
    pub affected_rows: u64,
    pub warnings: u16,
    pub elapsed_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOutput {
    pub results: Vec<StmtResult>,
    /// First failing statement stops the run.
    pub error: Option<AppError>,
    pub error_sql: Option<String>,
    /// DATABASE() after the run (changes with USE).
    pub schema: Option<String>,
}

/// Statements to run: all of them, or only the one under `cursor` (byte offset).
pub fn pick(sql: &str, cursor: Option<usize>) -> Vec<String> {
    let all = split(sql);
    match cursor {
        None => all.into_iter().map(|s| s.sql).collect(),
        // The statement containing the cursor, else the last one that ends before it.
        Some(c) => all.into_iter().rfind(|s| s.start <= c).map(|s| vec![s.sql]).unwrap_or_default(),
    }
}

pub async fn run(conn: &mut Conn, stmts: &[String], max_rows: usize) -> RunOutput {
    let mut out = RunOutput { results: Vec::new(), error: None, error_sql: None, schema: None };
    for sql in stmts {
        if let Err(e) = run_one(conn, sql, max_rows, &mut out.results).await {
            out.error = Some(e.into());
            out.error_sql = Some(sql.clone());
            break;
        }
    }
    out.schema = conn.query_first("SELECT DATABASE()").await.ok().flatten().flatten();
    out
}

async fn run_one(conn: &mut Conn, sql: &str, max_rows: usize, results: &mut Vec<StmtResult>) -> Result<(), mysql_async::Error> {
    let t = Instant::now();
    let mut res = conn.query_iter(sql).await?;
    let mut first = true;
    // One statement can return several result sets (CALL).
    loop {
        let columns: Vec<String> = res.columns_ref().iter().map(|c| c.name_str().into_owned()).collect();
        let (mut rows, mut total) = (Vec::new(), 0u64);
        // Streamed: rows past the cap are read and dropped, never kept in memory.
        // ponytail: a huge result is still read to the end; KILL QUERY at the cap if that gets slow.
        while let Some(row) = res.next().await? {
            total += 1;
            if rows.len() < max_rows {
                rows.push(row.unwrap().into_iter().map(cell).collect::<Vec<_>>());
            }
        }
        let r = StmtResult {
            sql: sql.to_string(),
            columns,
            rows,
            total_rows: total,
            affected_rows: res.affected_rows(),
            warnings: res.warnings(),
            elapsed_ms: t.elapsed().as_millis() as u64,
        };
        // A CALL ends with an empty OK packet: keep it only if nothing came before.
        if first || !r.columns.is_empty() || r.affected_rows > 0 {
            results.push(r);
        }
        first = false;
        if res.is_empty() {
            break;
        }
    }
    Ok(())
}

/// Stop the editor's running statement from another connection.
pub async fn cancel(pool: &Pool, conn_id: u32) -> AppResult<()> {
    let mut conn = pool.get_conn().await?;
    conn.query_drop(format!("KILL QUERY {conn_id}")).await?;
    Ok(())
}

/// First keyword of a statement, after comments and opening parentheses.
fn first_word(sql: &str) -> String {
    let mut s = sql;
    loop {
        s = s.trim_start().trim_start_matches('(');
        if s.starts_with("--") || s.starts_with('#') {
            s = s.find('\n').map_or("", |i| &s[i..]);
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.find("*/").map_or("", |i| &rest[i + 2..]);
        } else {
            break;
        }
    }
    s.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect::<String>().to_ascii_uppercase()
}

/// `ANALYZE` runs the statement for real: only a single SELECT (or WITH … SELECT) is wrapped.
pub fn analyze_sql(stmts: &[String]) -> AppResult<String> {
    match stmts {
        [one] if matches!(first_word(one).as_str(), "SELECT" | "WITH") => Ok(format!("ANALYZE FORMAT=JSON {one}")),
        _ => Err(AppError::new("ANALYZE_SELECT_ONLY", stmts.join(";\n"))),
    }
}

/// One step of a JSON path picked in the cell viewer: an object key or an array index.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum PathStep {
    Key(String),
    Index(usize),
}

fn json_path(steps: &[PathStep]) -> String {
    let mut p = String::from("$");
    for s in steps {
        match s {
            PathStep::Key(k) if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !k.starts_with(|c: char| c.is_ascii_digit()) => {
                p.push('.');
                p.push_str(k);
            }
            PathStep::Key(k) => p.push_str(&format!(".\"{}\"", k.replace('\\', "\\\\").replace('"', "\\\""))),
            PathStep::Index(i) => p.push_str(&format!("[{i}]")),
        }
    }
    p
}

/// SQL string literal. ponytail: backslashes doubled, wrong under NO_BACKSLASH_ESCAPES (only exotic keys have one).
fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
}

/// Starter `JSON_TABLE` query over the array at `path`, one column per scalar key of `sample` (an element).
pub fn json_table_sql(schema: &str, table: &str, column: &str, path: &[PathStep], sample: &serde_json::Value) -> String {
    use serde_json::Value;
    let mut cols = Vec::new();
    let mut nested = Vec::new();
    for (k, v) in sample.as_object().into_iter().flatten() {
        let ty = match v {
            Value::Bool(_) => "BOOLEAN",
            Value::Number(n) if n.is_i64() || n.is_u64() => "BIGINT",
            Value::Number(_) => "DOUBLE",
            Value::Object(_) | Value::Array(_) => {
                nested.push(format!("    -- {}: {}", k.replace('\n', " "), if v.is_array() { "NESTED PATH '...[*]' COLUMNS (...)" } else { "JSON_QUERY(...)" }));
                continue;
            }
            _ => "VARCHAR(255)",
        };
        cols.push(format!("    {} {ty} PATH {}", ident(k), literal(&json_path(&[PathStep::Key(k.clone())]))));
    }
    if cols.is_empty() {
        cols.push(format!("    {} LONGTEXT PATH '$'", ident("value")));
    }
    let mut body = cols.join(",\n");
    if !nested.is_empty() {
        body.push('\n');
        body.push_str(&nested.join("\n"));
    }
    format!(
        "SELECT jt.*\nFROM {}.{} AS t,\n  JSON_TABLE(t.{}, {} COLUMNS (\n{body}\n  )) AS jt\nLIMIT 100;",
        ident(schema),
        ident(table),
        ident(column),
        literal(&format!("{}[*]", json_path(path))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqls(s: &str) -> Vec<String> {
        split(s).into_iter().map(|s| s.sql).collect()
    }

    #[test]
    fn splits_on_semicolons_outside_quotes_and_comments() {
        assert_eq!(
            sqls("SELECT 'a;b', \"c;\", `d;`; -- x;y\nSELECT 1 /* ; */ ;# z;\nSELECT 'it''s;' ; SELECT 'a\\';'"),
            ["SELECT 'a;b', \"c;\", `d;`", "-- x;y\nSELECT 1 /* ; */", "# z;\nSELECT 'it''s;'", "SELECT 'a\\';'"]
        );
        assert_eq!(sqls("  ;; \n"), Vec::<String>::new());
        assert_eq!(sqls("SELECT 1"), ["SELECT 1"]);
        // "--" without a following space is an operator, not a comment.
        assert_eq!(sqls("SELECT 5--1; SELECT 2"), ["SELECT 5--1", "SELECT 2"]);
    }

    #[test]
    fn handles_delimiter_for_routine_bodies() {
        let src = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END//\ndelimiter ;\nCALL p();";
        assert_eq!(sqls(src), ["CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END", "CALL p()"]);
        let s = &split(src)[1];
        assert_eq!(&src[s.start..s.end], "CALL p()");
        // A comment before DELIMITER (as in dumps) does not hide it.
        let dump = "-- procedure p\nDELIMITER ;;\nCREATE PROCEDURE p() BEGIN SELECT 1; END;;\nDELIMITER ;\nSELECT 'DELIMITER x';";
        assert_eq!(sqls(dump), ["CREATE PROCEDURE p() BEGIN SELECT 1; END", "SELECT 'DELIMITER x'"]);
    }

    #[test]
    fn picks_statement_under_cursor() {
        let src = "SELECT 1;\nSELECT 2;\n\nSELECT 3";
        assert_eq!(pick(src, Some(0)), ["SELECT 1"]);
        assert_eq!(pick(src, Some(12)), ["SELECT 2"]);
        assert_eq!(pick(src, Some(src.len())), ["SELECT 3"]);
        assert_eq!(pick(src, None).len(), 3);
    }

    #[test]
    fn wraps_only_one_select_in_analyze() {
        assert_eq!(analyze_sql(&["SELECT 1".into()]).unwrap(), "ANALYZE FORMAT=JSON SELECT 1");
        assert!(analyze_sql(&["/* x */ -- y\n (WITH a AS (SELECT 1) SELECT * FROM a)".into()]).is_ok());
        for bad in [vec!["DELETE FROM t WHERE id = 1".to_string()], vec!["SELECT 1".into(), "SELECT 2".into()], vec![]] {
            assert_eq!(analyze_sql(&bad).unwrap_err().code, "ANALYZE_SELECT_ONLY");
        }
    }

    #[test]
    fn builds_json_table_from_a_sample() {
        let sample = serde_json::json!({"id": 1, "price": 9.5, "ok": true, "name": "x", "tags": [1], "o'k": null, "meta": {}});
        let path = [PathStep::Key("items".into()), PathStep::Index(0), PathStep::Key("a b".into())];
        assert_eq!(
            json_table_sql("s", "t`x", "doc", &path, &sample),
            "SELECT jt.*
FROM `s`.`t``x` AS t,
  JSON_TABLE(t.`doc`, '$.items[0].\"a b\"[*]' COLUMNS (
    `id` BIGINT PATH '$.id',
    `name` VARCHAR(255) PATH '$.name',
    `o'k` VARCHAR(255) PATH '$.\"o''k\"',
    `ok` BOOLEAN PATH '$.ok',
    `price` DOUBLE PATH '$.price'
    -- meta: JSON_QUERY(...)
    -- tags: NESTED PATH '...[*]' COLUMNS (...)
  )) AS jt
LIMIT 100;"
        );
        assert!(json_table_sql("s", "t", "c", &[], &serde_json::json!(3)).contains("`value` LONGTEXT PATH '$'"));
    }

    #[test]
    fn flags_destructive_statements() {
        assert!(risky("drop table t"));
        assert!(risky("TRUNCATE t"));
        assert!(risky("DELETE FROM t"));
        assert!(risky("update t set a=1"));
        assert!(!risky("UPDATE t SET a=1 WHERE id=2"));
        assert!(!risky("SELECT * FROM t"));
    }

    /// OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn runs_and_cancels_on_test_server() {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let opts = mysql_async::OptsBuilder::default()
            .ip_or_hostname(p[0])
            .tcp_port(p[1].parse().unwrap())
            .user(Some(p[2]))
            .pass(Some(p[3]))
            .prefer_socket(false);
        let pool = Pool::new(opts);
        let ed = open_editor(&pool, Some("mysql")).await.unwrap();
        let mut conn = ed.conn.lock().await;
        let out = run(&mut conn, &pick("SELECT 1 a UNION SELECT 2; DO 1; SELECT x", None), 1).await;
        assert_eq!(out.results.len(), 2);
        assert_eq!((out.results[0].rows.len(), out.results[0].total_rows), (1, 2));
        assert_eq!(out.error.unwrap().server_code, Some(1054));
        assert_eq!(out.schema.as_deref(), Some("mysql"));

        let id = ed.conn_id;
        let p2 = pool.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            cancel(&p2, id).await.unwrap();
        });
        let out = run(&mut conn, &["SELECT SLEEP(5)".into()], 10).await;
        // SLEEP returns 1 when interrupted rather than failing.
        assert!(out.error.is_some() || out.results[0].rows[0][0].as_deref() == Some("1"));
    }
}
