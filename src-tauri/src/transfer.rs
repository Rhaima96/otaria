//! Import / export. Rows go from the server to the file (and back) in Rust, never through the UI.
//! A table or view → CSV, JSON, Excel or SQL; a whole schema → SQL; an SQL file → the server, streamed.
// ponytail: internal SQL dump only. mariadb-dump would need the password, the SSH tunnel port and the
// TLS files on its command line; add it if the internal dump falls short.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use mysql_async::consts::ColumnType;
use mysql_async::prelude::Queryable;
use mysql_async::{Column, Conn, Pool, QueryResult, Row, TextProtocol, TxOpts, Value};
use serde::Deserialize;
use tokio::io::AsyncReadExt;

use crate::browse::{self, ident};
use crate::error::{AppError, AppResult};
use crate::query;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Csv,
    Json,
    Xlsx,
    Sql,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReq {
    pub schema: String,
    /// None = the whole schema (SQL only).
    pub table: Option<String>,
    pub format: Format,
    pub path: String,
    /// SQL: include the rows (else structure only).
    pub data: bool,
    /// SQL: `DROP … IF EXISTS` before each CREATE.
    pub drop: bool,
}

/// Rows between two progress reports.
const REPORT_EVERY: u64 = 5000;
/// One INSERT per this many rows, or per MiB of SQL, whichever comes first.
const INSERT_ROWS: usize = 1000;
const INSERT_BYTES: usize = 1 << 20;
/// Excel limits.
const XLSX_ROWS: u32 = 1_048_576;
const XLSX_CELL_CHARS: usize = 32_767;

fn file_err(e: impl std::fmt::Display) -> AppError {
    AppError::new("IO", e.to_string())
}

fn cancelled() -> AppError {
    AppError::new("QUERY_CANCELLED", "")
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Num,
    Bin,
    Text,
}

/// How a column's values are written. Temporal types also report the binary charset (63): only string
/// and BLOB types with it hold raw bytes.
fn kind(c: &Column) -> Kind {
    use ColumnType::*;
    match c.column_type() {
        MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_LONG | MYSQL_TYPE_LONGLONG | MYSQL_TYPE_INT24 | MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE
        | MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL | MYSQL_TYPE_YEAR => Kind::Num,
        MYSQL_TYPE_BIT => Kind::Bin,
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_STRING | MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB
        | MYSQL_TYPE_BLOB | MYSQL_TYPE_GEOMETRY
            if c.character_set() == 63 =>
        {
            Kind::Bin
        }
        _ => Kind::Text,
    }
}

/// SQL literal for a dump. The dump header leaves NO_BACKSLASH_ESCAPES off, so backslash escapes are valid.
fn sql_literal(v: &Value, k: Kind) -> String {
    match v {
        Value::NULL => "NULL".into(),
        Value::Bytes(b) => match k {
            Kind::Num => String::from_utf8_lossy(b).into_owned(),
            Kind::Bin if b.is_empty() => "''".into(),
            Kind::Bin => b.iter().fold(String::from("0x"), |s, c| s + &format!("{c:02X}")),
            Kind::Text => quote(&String::from_utf8_lossy(b)),
        },
        // The text protocol only sends bytes; binary-protocol values keep a sane fallback.
        other => other.as_sql(false),
    }
}

fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('\'');
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\'' => o.push_str("\\'"),
            '\0' => o.push_str("\\0"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\x1a' => o.push_str("\\Z"),
            c => o.push(c),
        }
    }
    o.push('\'');
    o
}

/// All columns in table order (invisible ones included, `SELECT *` would skip them), or only those an
/// INSERT accepts: generated columns and ROW START / ROW END have a GENERATION_EXPRESSION.
async fn column_names(q: &mut impl Queryable, schema: &str, table: &str, insertable: bool) -> AppResult<Vec<String>> {
    let filter = if insertable { "AND (GENERATION_EXPRESSION IS NULL OR GENERATION_EXPRESSION = '')" } else { "" };
    Ok(q.exec(
        format!("SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? {filter} ORDER BY ORDINAL_POSITION"),
        (schema, table),
    )
    .await?)
}

fn select(schema: &str, table: &str, cols: &[String]) -> String {
    let list: Vec<String> = cols.iter().map(|c| ident(c)).collect();
    format!("SELECT {} FROM {}.{}", list.join(", "), ident(schema), ident(table))
}

/// Run an export; a failed or cancelled one leaves no partial file behind. Returns the number of rows.
pub async fn export(pool: &Pool, req: &ExportReq, cancel: &AtomicBool, progress: impl Fn(u64)) -> AppResult<u64> {
    let r = match (req.format, req.table.as_deref()) {
        (Format::Sql, table) => export_sql(pool, req, table, cancel, &progress).await,
        (_, Some(table)) => export_rows(pool, req, table, cancel, &progress).await,
        (_, None) => Err(AppError::new("EXPORT_FORMAT", "")),
    };
    if r.is_err() {
        let _ = std::fs::remove_file(&req.path);
    }
    r
}

// ponytail: file writes are blocking calls inside the async task; fine at these sizes, move to
// spawn_blocking if exports ever stall other commands.
async fn export_rows(pool: &Pool, req: &ExportReq, table: &str, cancel: &AtomicBool, progress: &impl Fn(u64)) -> AppResult<u64> {
    let mut conn = pool.get_conn().await?;
    let cols = column_names(&mut conn, &req.schema, table, false).await?;
    if cols.is_empty() {
        return Err(AppError::new("NOT_FOUND", table.to_string()));
    }
    let mut res = conn.query_iter(select(&req.schema, table, &cols)).await?;
    let kinds: Vec<Kind> = res.columns_ref().iter().map(kind).collect();
    let mut n: u64 = 0;
    let next = async |res: &mut QueryResult<'_, 'static, TextProtocol>, n: &mut u64| -> AppResult<Option<Vec<Option<String>>>> {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let row: Option<Row> = res.next().await?;
        if row.is_some() {
            *n += 1;
            if n.is_multiple_of(REPORT_EVERY) {
                progress(*n);
            }
        }
        Ok(row.map(|r| r.unwrap().into_iter().map(browse::cell).collect()))
    };

    match req.format {
        Format::Csv => {
            // ponytail: NULL and '' both become an empty field; write \N if a reader needs to tell them apart.
            let mut w = csv::Writer::from_path(&req.path).map_err(file_err)?;
            w.write_record(&cols).map_err(file_err)?;
            while let Some(row) = next(&mut res, &mut n).await? {
                w.write_record(row.iter().map(|c| c.as_deref().unwrap_or(""))).map_err(file_err)?;
            }
            w.flush()?;
        }
        Format::Json => {
            let mut w = BufWriter::new(File::create(&req.path)?);
            let keys: Vec<String> = cols.iter().map(|c| serde_json::to_string(c).unwrap_or_default()).collect();
            w.write_all(b"[")?;
            let mut first = true;
            while let Some(row) = next(&mut res, &mut n).await? {
                w.write_all(if first { b"\n  {" } else { b",\n  {" })?;
                first = false;
                for (i, c) in row.iter().enumerate() {
                    if i > 0 {
                        w.write_all(b", ")?;
                    }
                    write!(w, "{}: {}", keys[i], json_value(c.as_deref(), kinds[i]))?;
                }
                w.write_all(b"}")?;
            }
            w.write_all(if first { b"]\n" } else { b"\n]\n" })?;
            w.flush()?;
        }
        Format::Xlsx => {
            use rust_xlsxwriter::{Format as XFormat, Workbook};
            let mut book = Workbook::new();
            let bold = XFormat::new().set_bold();
            let sheet = book.add_worksheet_with_constant_memory().map_err(file_err)?;
            sheet.set_name(sheet_name(table)).map_err(file_err)?;
            for (i, c) in cols.iter().enumerate() {
                sheet.write_string_with_format(0, i as u16, c, &bold).map_err(file_err)?;
            }
            let mut r: u32 = 1;
            while let Some(row) = next(&mut res, &mut n).await? {
                if r >= XLSX_ROWS {
                    return Err(AppError::new("EXPORT_TOO_MANY_ROWS", XLSX_ROWS.to_string()));
                }
                for (i, c) in row.iter().enumerate() {
                    let Some(s) = c.as_deref() else { continue };
                    match xlsx_number(s, kinds[i]) {
                        Some(x) => sheet.write_number(r, i as u16, x),
                        // ponytail: cells over Excel's 32,767 characters are cut; CSV/JSON keep them whole.
                        None => sheet.write_string(r, i as u16, s.chars().take(XLSX_CELL_CHARS).collect::<String>()),
                    }
                    .map_err(file_err)?;
                }
                r += 1;
            }
            book.save(&req.path).map_err(file_err)?;
        }
        Format::Sql => unreachable!("handled by export_sql"),
    }
    Ok(n)
}

/// Numbers stay numbers when JSON can hold them exactly as the server wrote them.
fn json_value(v: Option<&str>, k: Kind) -> String {
    match v {
        None => "null".into(),
        Some(s) if k == Kind::Num && serde_json::from_str::<serde_json::Number>(s).is_ok() => s.to_string(),
        Some(s) => serde_json::to_string(s).unwrap_or_default(),
    }
}

/// Excel stores numbers as f64: past 15 significant digits they are kept as text so nothing is rounded.
fn xlsx_number(s: &str, k: Kind) -> Option<f64> {
    if k != Kind::Num || s.chars().filter(char::is_ascii_digit).count() > 15 {
        return None;
    }
    s.parse::<f64>().ok().filter(|x| x.is_finite())
}

/// Sheet names: at most 31 characters, none of `[]:*?/\`.
fn sheet_name(table: &str) -> String {
    let name: String = table.chars().map(|c| if "[]:*?/\\".contains(c) { '_' } else { c }).take(31).collect();
    if name.trim().is_empty() { "Sheet1".into() } else { name }
}

const HEADER: &str = "SET NAMES utf8mb4;
SET @OLD_FOREIGN_KEY_CHECKS = @@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS = 0;
SET @OLD_UNIQUE_CHECKS = @@UNIQUE_CHECKS, UNIQUE_CHECKS = 0;
SET @OLD_SQL_MODE = @@SQL_MODE, SQL_MODE = 'NO_AUTO_VALUE_ON_ZERO';
SET @OLD_TIME_ZONE = @@TIME_ZONE, TIME_ZONE = '+00:00';
";
const FOOTER: &str = "
SET TIME_ZONE = @OLD_TIME_ZONE;
SET SQL_MODE = @OLD_SQL_MODE;
SET UNIQUE_CHECKS = @OLD_UNIQUE_CHECKS;
SET FOREIGN_KEY_CHECKS = @OLD_FOREIGN_KEY_CHECKS;
";

/// Objects in an order that recreates them: sequences before the tables whose defaults use them,
/// views after tables, triggers after the rows (so they do not fire on the INSERTs).
// ponytail: views on views are created alphabetically; one may fail if it sorts before the view it reads.
const KIND_ORDER: [&str; 7] = ["sequence", "table", "view", "procedure", "function", "trigger", "event"];

/// Schema (or one table) as an SQL script with no schema name in it, so it can be loaded into any schema.
/// Rows are read in one consistent snapshot, timestamps in UTC like the header says.
async fn export_sql(pool: &Pool, req: &ExportReq, table: Option<&str>, cancel: &AtomicBool, progress: &impl Fn(u64)) -> AppResult<u64> {
    let schema = req.schema.as_str();
    let mut objects: Vec<(String, String)> = match table {
        Some(t) => vec![("table".into(), t.to_string())],
        None => browse::objects(pool, schema).await?.into_iter().map(|o| (o.kind, o.name)).collect(),
    };
    objects.sort_by_key(|(kind, _)| KIND_ORDER.iter().position(|k| k == kind).unwrap_or(KIND_ORDER.len()));

    let mut conn = pool.get_conn().await?;
    let mut opts = TxOpts::default();
    opts.with_consistent_snapshot(true).with_readonly(true);
    let mut tx = conn.start_transaction(opts).await?;
    tx.query_drop(format!("SET time_zone = '+00:00'; USE {}", ident(schema))).await?;

    let mut w = BufWriter::new(File::create(&req.path)?);
    w.write_all(HEADER.as_bytes())?;
    let mut rows: u64 = 0;
    for (kind, name) in &objects {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let ddl = browse::ddl_on(&mut tx, schema, name, kind).await?;
        let keyword = kind.to_ascii_uppercase();
        writeln!(w, "\n-- {kind} {}", ident(name))?;
        if req.drop {
            writeln!(w, "DROP {keyword} IF EXISTS {};", ident(name))?;
        }
        match kind.as_str() {
            "procedure" | "function" | "trigger" | "event" => writeln!(w, "DELIMITER ;;\n{ddl};;\nDELIMITER ;")?,
            _ => writeln!(w, "{ddl};")?,
        }
        if kind == "sequence" {
            let next: Option<String> = tx.query_first(format!("SELECT CAST(next_not_cached_value AS CHAR) FROM {}.{}", ident(schema), ident(name))).await?;
            if let Some(n) = next {
                writeln!(w, "SELECT SETVAL({}, {n}, 0);", ident(name))?;
            }
        }
        if kind == "table" && req.data {
            rows += dump_rows(&mut tx, &mut w, schema, name, cancel, progress, rows).await?;
        }
    }
    w.write_all(FOOTER.as_bytes())?;
    w.flush()?;
    tx.commit().await?;
    Ok(rows)
}

async fn dump_rows(
    q: &mut impl Queryable,
    w: &mut impl Write,
    schema: &str,
    table: &str,
    cancel: &AtomicBool,
    progress: &impl Fn(u64),
    before: u64,
) -> AppResult<u64> {
    let cols = column_names(q, schema, table, true).await?;
    if cols.is_empty() {
        return Ok(0);
    }
    let list: Vec<String> = cols.iter().map(|c| ident(c)).collect();
    let head = format!("INSERT INTO {} ({}) VALUES\n", ident(table), list.join(", "));
    let mut res = q.query_iter(select(schema, table, &cols)).await?;
    let kinds: Vec<Kind> = res.columns_ref().iter().map(kind).collect();
    let (mut buf, mut in_buf, mut n) = (String::new(), 0usize, 0u64);
    while let Some(row) = res.next().await? {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let row: Row = row;
        buf.push_str(if in_buf == 0 { &head } else { ",\n" });
        buf.push('(');
        for (i, v) in row.unwrap().iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            buf.push_str(&sql_literal(v, kinds[i]));
        }
        buf.push(')');
        in_buf += 1;
        n += 1;
        if in_buf >= INSERT_ROWS || buf.len() >= INSERT_BYTES {
            buf.push_str(";\n");
            w.write_all(buf.as_bytes())?;
            buf.clear();
            in_buf = 0;
        }
        if (before + n).is_multiple_of(REPORT_EVERY) {
            progress(before + n);
        }
    }
    if in_buf > 0 {
        buf.push_str(";\n");
        w.write_all(buf.as_bytes())?;
    }
    Ok(n)
}

/// Run an SQL file on `conn`, read in chunks so a large dump never sits in memory whole.
/// Stops at the first failing statement (the ones before it stay applied, as with the mariadb client).
/// `progress(bytes read, file size)`. Returns the number of statements run.
pub async fn import_sql(conn: &mut Conn, path: &str, cancel: &AtomicBool, progress: impl Fn(u64, u64)) -> AppResult<u64> {
    let mut file = tokio::fs::File::open(path).await?;
    let total = file.metadata().await?.len();
    let mut chunk = vec![0u8; 1 << 20];
    let (mut pending, mut text, mut delim) = (Vec::<u8>::new(), String::new(), ";".to_string());
    let (mut read, mut n) = (0u64, 0u64);
    loop {
        let got = file.read(&mut chunk).await?;
        read += got as u64;
        pending.extend_from_slice(&chunk[..got]);
        let eof = got == 0;
        // Parse only up to the last newline: DELIMITER lines and UTF-8 sequences are never cut.
        let cut = if eof { pending.len() } else { pending.iter().rposition(|&c| c == b'\n').map_or(0, |p| p + 1) };
        if cut > 0 {
            let piece: Vec<u8> = pending.drain(..cut).collect();
            text.push_str(&String::from_utf8(piece).map_err(|_| AppError::new("FILE_NOT_UTF8", path))?);
            // ponytail: an unfinished statement is parsed again with each new chunk; quadratic only for a
            // single statement of many MiB.
            let (stmts, tail, d) = query::split_from(&text, &delim);
            for s in &stmts {
                n += 1;
                run(conn, &s.sql, n, cancel).await?;
            }
            text.drain(..tail);
            delim = d;
        }
        progress(read, total);
        if eof {
            break;
        }
    }
    let last = text.trim();
    if !last.is_empty() {
        n += 1;
        run(conn, last, n, cancel).await?;
    }
    Ok(n)
}

async fn run(conn: &mut Conn, sql: &str, n: u64, cancel: &AtomicBool) -> AppResult<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(cancelled());
    }
    conn.query_drop(sql).await.map_err(|e| {
        let mut err = AppError::from(e);
        if err.code != "QUERY_CANCELLED" {
            err.code = "IMPORT_STATEMENT";
        }
        let snippet: String = sql.chars().take(300).collect();
        err.detail = Some(format!("#{n}: {snippet}"));
        err
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_sql_literals() {
        let b = |s: &[u8]| Value::Bytes(s.to_vec());
        assert_eq!(sql_literal(&Value::NULL, Kind::Text), "NULL");
        assert_eq!(sql_literal(&b(b"-12.50"), Kind::Num), "-12.50");
        assert_eq!(sql_literal(&b(b"it's a \\ \n\r\0\x1a"), Kind::Text), r"'it\'s a \\ \n\r\0\Z'");
        assert_eq!(sql_literal(&b(&[0, 255, 16]), Kind::Bin), "0x00FF10");
        assert_eq!(sql_literal(&b(b""), Kind::Bin), "''");
    }

    #[test]
    fn keeps_numbers_exact() {
        assert_eq!(json_value(Some("12.50"), Kind::Num), "12.50");
        assert_eq!(json_value(Some("12345678901234567890"), Kind::Num), "12345678901234567890");
        assert_eq!(json_value(Some("007"), Kind::Text), "\"007\"");
        assert_eq!(json_value(None, Kind::Num), "null");
        assert_eq!(xlsx_number("123456789012345", Kind::Num), Some(123456789012345.0));
        assert_eq!(xlsx_number("1234567890123456", Kind::Num), None);
        assert_eq!(xlsx_number("42", Kind::Text), None);
        assert_eq!(sheet_name("a/b:c"), "a_b_c");
    }

    /// The SQL file is fed in small pieces to check that statements cut across chunks, DELIMITER blocks
    /// and quoted delimiters survive the streaming split (same loop as `import_sql`, without a server).
    #[test]
    fn splits_a_stream() {
        let src = "CREATE TABLE t (a TEXT);\nINSERT INTO t VALUES ('x;\ny');\nDELIMITER ;;\nCREATE PROCEDURE p() BEGIN SELECT 1; END;;\nDELIMITER ;\nSELECT 2";
        for size in [1, 7, 40, 1000] {
            let (mut text, mut delim, mut got) = (String::new(), ";".to_string(), Vec::new());
            let mut pending = String::new();
            for (i, c) in src.chars().enumerate() {
                pending.push(c);
                let eof = i + 1 == src.len();
                if pending.len() < size && !eof {
                    continue;
                }
                let cut = if eof { pending.len() } else { pending.rfind('\n').map_or(0, |p| p + 1) };
                text.push_str(&pending.drain(..cut).collect::<String>());
                let (stmts, tail, d) = query::split_from(&text, &delim);
                got.extend(stmts.into_iter().map(|s| s.sql));
                text.drain(..tail);
                delim = d;
            }
            if !text.trim().is_empty() {
                got.push(text.trim().to_string());
            }
            assert_eq!(
                got,
                ["CREATE TABLE t (a TEXT)", "INSERT INTO t VALUES ('x;\ny')", "CREATE PROCEDURE p() BEGIN SELECT 1; END", "SELECT 2"],
                "chunk size {size}"
            );
        }
    }

    /// OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn round_trip_on_test_server() {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let pool = Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        );
        let mut conn = pool.get_conn().await.unwrap();
        conn.query_drop(
            "DROP DATABASE IF EXISTS otaria_ex; DROP DATABASE IF EXISTS otaria_im; CREATE DATABASE otaria_ex; CREATE DATABASE otaria_im;
             CREATE TABLE otaria_ex.t (id INT PRIMARY KEY, txt TEXT, bin VARBINARY(8), d DECIMAL(30,2), ts TIMESTAMP NULL,
               g INT AS (id * 2) VIRTUAL, inv INT INVISIBLE DEFAULT 5) WITH SYSTEM VERSIONING;
             INSERT INTO otaria_ex.t (id, txt, bin, d, ts, inv) VALUES (1, 'a''b\\\\c\\n;', 0x00FF, 12345678901234567890.25, '2026-01-02 03:04:05', 9),
               (2, NULL, NULL, NULL, NULL, 5);
             CREATE SEQUENCE otaria_ex.s START WITH 10 NOCACHE; SELECT NEXTVAL(otaria_ex.s);
             CREATE VIEW otaria_ex.v AS SELECT id FROM otaria_ex.t;
             USE otaria_ex;
             CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END;
             CREATE TRIGGER tr BEFORE INSERT ON t FOR EACH ROW SET NEW.txt = UPPER(NEW.txt);",
        )
        .await
        .unwrap();
        let dir = std::env::temp_dir();
        let path = |ext: &str| dir.join(format!("otaria_test.{ext}")).to_string_lossy().into_owned();
        let req = |format, table: Option<&str>, ext| ExportReq {
            schema: "otaria_ex".into(),
            table: table.map(Into::into),
            format,
            path: path(ext),
            data: true,
            drop: false,
        };
        let no = AtomicBool::new(false);

        // Whole schema → SQL → another schema: rows (escapes, bytes, big decimal, invisible column), sequence, view, routine, trigger.
        assert_eq!(export(&pool, &req(Format::Sql, None, "sql"), &no, |_| {}).await.unwrap(), 2);
        let mut im = pool.get_conn().await.unwrap();
        im.query_drop("USE otaria_im").await.unwrap();
        assert!(import_sql(&mut im, &path("sql"), &no, |_, _| {}).await.unwrap() > 10);
        let same: Option<u8> = conn
            .query_first(
                "SELECT COUNT(*) = 2 FROM otaria_ex.t a JOIN otaria_im.t b USING (id)
                  WHERE a.txt <=> b.txt AND a.bin <=> b.bin AND a.d <=> b.d AND a.ts <=> b.ts AND a.g = b.g AND a.inv = b.inv",
            )
            .await
            .unwrap();
        assert_eq!(same, Some(1));
        let next: Option<u64> = conn.query_first("SELECT NEXTVAL(otaria_im.s)").await.unwrap();
        assert_eq!(next, Some(11));
        let objs: Option<u64> = conn
            .query_first("SELECT (SELECT COUNT(*) FROM information_schema.VIEWS WHERE TABLE_SCHEMA = 'otaria_im') + (SELECT COUNT(*) FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = 'otaria_im') + (SELECT COUNT(*) FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = 'otaria_im')")
            .await
            .unwrap();
        assert_eq!(objs, Some(3));

        // Rows → CSV / JSON / Excel.
        assert_eq!(export(&pool, &req(Format::Csv, Some("t"), "csv"), &no, |_| {}).await.unwrap(), 2);
        let csv = std::fs::read_to_string(path("csv")).unwrap();
        assert!(csv.starts_with("id,txt,bin,d,ts,g,inv\n1,\"a'b\\c\n;\","), "{csv}");
        assert_eq!(export(&pool, &req(Format::Json, Some("t"), "json"), &no, |_| {}).await.unwrap(), 2);
        let text = std::fs::read_to_string(path("json")).unwrap();
        assert!(text.contains(r#""d": 12345678901234567890.25"#), "{text}");
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json[1]["txt"], serde_json::Value::Null);
        assert_eq!(export(&pool, &req(Format::Xlsx, Some("t"), "xlsx"), &no, |_| {}).await.unwrap(), 2);
        assert!(std::fs::metadata(path("xlsx")).unwrap().len() > 1000);

        // A failing statement stops the import and says which one; a cancelled export leaves no file.
        std::fs::write(path("bad.sql"), "SELECT 1;\nSELEC 2;\nSELECT 3;").unwrap();
        let err = import_sql(&mut im, &path("bad.sql"), &no, |_, _| {}).await.unwrap_err();
        assert_eq!((err.code, err.detail.as_deref()), ("IMPORT_STATEMENT", Some("#2: SELEC 2")));
        let stop = AtomicBool::new(true);
        assert_eq!(export(&pool, &req(Format::Csv, Some("t"), "stop.csv"), &stop, |_| {}).await.unwrap_err().code, "QUERY_CANCELLED");
        assert!(!std::path::Path::new(&path("stop.csv")).exists());

        conn.query_drop("DROP DATABASE otaria_ex; DROP DATABASE otaria_im").await.unwrap();
    }
}
