//! Import / export. Rows go from the server to the file (and back) in Rust, never through the UI.
//! A table or view → CSV, JSON, Excel or SQL; a whole schema → SQL; an SQL file → the server, streamed;
//! a CSV file → a table (batched INSERTs in one transaction); a table or schema → another connection.
// ponytail: internal SQL dump only. mariadb-dump would need the password, the SSH tunnel port and the
// TLS files on its command line; add it if the internal dump falls short.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use mysql_async::consts::ColumnType;
use mysql_async::prelude::Queryable;
use mysql_async::{Column, Conn, Pool, QueryResult, Row, TextProtocol, TxOpts, Value};
use serde::{Deserialize, Serialize};
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
    /// CSV: NULL written `\N` (as `SELECT … INTO OUTFILE` does), so it differs from an empty string.
    #[serde(default)]
    pub null_as_n: bool,
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
            let null = if req.null_as_n { "\\N" } else { "" };
            let mut w = csv::Writer::from_path(&req.path).map_err(file_err)?;
            w.write_record(&cols).map_err(file_err)?;
            while let Some(row) = next(&mut res, &mut n).await? {
                w.write_record(row.iter().map(|c| c.as_deref().unwrap_or(null))).map_err(file_err)?;
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
/// views after tables (each after the views it reads), triggers after the rows (so they do not fire on the INSERTs).
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

    // Views: read their text first to put each after the views it reads.
    let mut views = Vec::new();
    for (_, name) in objects.iter().filter(|(k, _)| k == "view") {
        views.push((name.clone(), browse::ddl_on(&mut tx, schema, name, "view").await?));
    }
    if !views.is_empty() {
        let at = objects.iter().position(|(k, _)| k == "view").unwrap_or(0);
        objects.retain(|(k, _)| k != "view");
        let ordered = browse::view_order(&views).into_iter().map(|i| ("view".to_string(), views[i].0.clone()));
        objects.splice(at..at, ordered);
    }
    let mut view_ddl: HashMap<String, String> = views.into_iter().collect();

    let mut w = BufWriter::new(File::create(&req.path)?);
    w.write_all(HEADER.as_bytes())?;
    let mut rows: u64 = 0;
    for (kind, name) in &objects {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let ddl = match view_ddl.remove(name) {
            Some(d) => d,
            None => browse::ddl_on(&mut tx, schema, name, kind).await?,
        };
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

/// First rows of a CSV file, to map its columns before importing.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvPreview {
    /// Header names, or "1", "2"… without a header row.
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// The delimiter used (detected when none was given).
    pub delimiter: String,
}

const PREVIEW_ROWS: usize = 20;

/// The most frequent of `,` `;` tab `|` on the first line; `,` when none appears (ties go to the first).
fn detect_delimiter(first_line: &str) -> u8 {
    let count = |d: u8| first_line.bytes().filter(|&c| c == d).count();
    b";\t|".iter().fold(b',', |best, &d| if count(d) > count(best) { d } else { best })
}

fn delimiter_byte(d: &str) -> AppResult<u8> {
    match d.as_bytes() {
        [b] if b.is_ascii() && !b.is_ascii_alphanumeric() && *b != b'"' => Ok(*b),
        _ => Err(AppError::new("CSV_DELIMITER", d.to_string())),
    }
}

fn csv_err(e: csv::Error) -> AppError {
    let line = e.position().map(|p| p.line());
    let code = if matches!(e.kind(), csv::ErrorKind::Utf8 { .. }) { "FILE_NOT_UTF8" } else { "CSV_PARSE" };
    AppError { detail: line.map(|l| l.to_string()), ..AppError::new(code, e.to_string()) }
}

fn csv_reader(path: &str, delimiter: u8, header: bool) -> AppResult<csv::Reader<File>> {
    csv::ReaderBuilder::new().delimiter(delimiter).has_headers(header).from_path(path).map_err(csv_err)
}

pub fn csv_preview(path: &str, delimiter: Option<&str>, header: bool) -> AppResult<CsvPreview> {
    let d = match delimiter {
        Some(d) => delimiter_byte(d)?,
        None => {
            let mut first = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(File::open(path)?), &mut first)?;
            detect_delimiter(&first)
        }
    };
    let mut rd = csv_reader(path, d, header)?;
    let rows: Vec<Vec<String>> =
        rd.records().take(PREVIEW_ROWS).map(|r| r.map(|r| r.iter().map(str::to_string).collect())).collect::<Result<_, _>>().map_err(csv_err)?;
    let columns = if header {
        // A UTF-8 BOM written by Excel would stick to the first name.
        rd.headers().map_err(csv_err)?.iter().enumerate().map(|(i, h)| if i == 0 { h.trim_start_matches('\u{feff}') } else { h }.to_string()).collect()
    } else {
        (1..=rows.first().map_or(0, Vec::len)).map(|i| i.to_string()).collect()
    };
    Ok(CsvPreview { columns, rows, delimiter: (d as char).to_string() })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvImport {
    pub schema: String,
    pub table: String,
    pub path: String,
    pub delimiter: String,
    pub header: bool,
    /// Target column for each CSV column, in file order; None = skip that CSV column.
    pub mapping: Vec<Option<String>>,
    /// Empty field → NULL (what the CSV export writes for NULL).
    pub empty_null: bool,
}

/// Placeholders per statement: the protocol allows 65,535.
const MAX_PARAMS: usize = 65_535;

/// The INSERT for `rows` rows of the mapped columns (values bound as parameters).
fn insert_sql(schema: &str, table: &str, cols: &[&str], rows: usize) -> String {
    let list: Vec<String> = cols.iter().map(|c| ident(c)).collect();
    let one = format!("({})", vec!["?"; cols.len()].join(", "));
    format!("INSERT INTO {}.{} ({}) VALUES {}", ident(schema), ident(table), list.join(", "), vec![one; rows].join(", "))
}

/// CSV → table in one transaction: any error (bad line, server refusal) rolls everything back.
/// `progress(bytes read, file size)`. Returns the number of rows inserted.
pub async fn import_csv(pool: &Pool, req: &CsvImport, cancel: &AtomicBool, progress: impl Fn(u64, u64)) -> AppResult<u64> {
    let mut conn = pool.get_conn().await?;
    let known = column_names(&mut conn, &req.schema, &req.table, true).await?;
    let picked: Vec<(usize, &str)> = req.mapping.iter().enumerate().filter_map(|(i, m)| m.as_deref().map(|c| (i, c))).collect();
    if picked.is_empty() {
        return Err(AppError::new("REQUIRED", "mapping"));
    }
    if let Some((_, c)) = picked.iter().find(|(_, c)| !known.iter().any(|k| k == c)) {
        return Err(AppError::new("NOT_FOUND", c.to_string()));
    }
    let cols: Vec<&str> = picked.iter().map(|(_, c)| *c).collect();
    let batch = (MAX_PARAMS / cols.len()).min(INSERT_ROWS);
    let full_sql = insert_sql(&req.schema, &req.table, &cols, batch);

    let total = std::fs::metadata(&req.path)?.len();
    let mut rd = csv_reader(&req.path, delimiter_byte(&req.delimiter)?, req.header)?;
    let mut tx = conn.start_transaction(TxOpts::default()).await?;
    let (mut params, mut in_batch, mut first_line, mut last_line, mut n) = (Vec::<Value>::new(), 0usize, 0u64, 0u64, 0u64);
    let mut record = csv::StringRecord::new();
    loop {
        let more = rd.read_record(&mut record).map_err(csv_err)?;
        if more {
            if cancel.load(Ordering::Relaxed) {
                return Err(cancelled()); // dropping `tx` rolls back
            }
            last_line = record.position().map_or(0, |p| p.line());
            if in_batch == 0 {
                first_line = last_line;
            }
            for (i, _) in &picked {
                let v = record.get(*i).unwrap_or("");
                // `\N` is NULL, as in LOAD DATA and in our own CSV export.
                params.push(if v == "\\N" || (v.is_empty() && req.empty_null) { Value::NULL } else { Value::from(v) });
            }
            in_batch += 1;
        }
        if in_batch > 0 && (in_batch == batch || !more) {
            let sql = if in_batch == batch { full_sql.clone() } else { insert_sql(&req.schema, &req.table, &cols, in_batch) };
            tx.exec_drop(sql, std::mem::take(&mut params)).await.map_err(|e| {
                // The server says "at row k" within the batch; the line range locates it in the file.
                let err = AppError::from(e);
                let code = if err.code == "QUERY_CANCELLED" { err.code } else { "IMPORT_ROWS" };
                AppError { code, detail: Some(format!("{first_line}-{last_line}")), ..err }
            })?;
            n += in_batch as u64;
            in_batch = 0;
            progress(record.position().map_or(0, |p| p.byte()), total);
        }
        if !more {
            break;
        }
    }
    tx.commit().await?;
    Ok(n)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyReq {
    pub schema: String,
    /// None = the whole schema.
    pub table: Option<String>,
    pub target_session: String,
    /// Created if it does not exist.
    pub target_schema: String,
    pub data: bool,
    pub drop: bool,
}

/// Copy = SQL export to a temporary file, then import on the target, so it works between any two
/// servers (and MySQL ↔ MariaDB as far as the DDL allows). The file is removed whatever happens.
/// `progress(done, total)`: rows exported (total 0), then bytes imported.
// ponytail: goes through a temp file on disk; stream directly between the two connections if disk space matters.
pub async fn copy(src: &Pool, dst: &Pool, req: &CopyReq, cancel: &AtomicBool, progress: impl Fn(u64, u64)) -> AppResult<u64> {
    let path = std::env::temp_dir().join(format!("otaria-copy-{}.sql", uuid::Uuid::new_v4())).to_string_lossy().into_owned();
    let export_req = ExportReq { schema: req.schema.clone(), table: req.table.clone(), format: Format::Sql, path: path.clone(), data: req.data, drop: req.drop, null_as_n: false };
    let r = async {
        let rows = export(src, &export_req, cancel, |n| progress(n, 0)).await?;
        let mut conn = dst.get_conn().await?;
        conn.query_drop(format!("CREATE DATABASE IF NOT EXISTS {}", ident(&req.target_schema))).await?;
        conn.query_drop(format!("USE {}", ident(&req.target_schema))).await?;
        import_sql(&mut conn, &path, cancel, &progress).await?;
        Ok(rows)
    }
    .await;
    let _ = std::fs::remove_file(&path);
    r
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
    fn prepares_csv_import() {
        assert_eq!(detect_delimiter("a;b;c,d"), b';');
        assert_eq!(detect_delimiter("a\tb\tc"), b'\t');
        assert_eq!(detect_delimiter("single"), b',');
        assert_eq!(delimiter_byte("|").unwrap(), b'|');
        for bad in ["", ";;", "a", "\"", "é"] {
            assert_eq!(delimiter_byte(bad).unwrap_err().code, "CSV_DELIMITER", "{bad}");
        }
        assert_eq!(insert_sql("s", "t", &["a", "b`c"], 2), "INSERT INTO `s`.`t` (`a`, `b``c`) VALUES (?, ?), (?, ?)");
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
            "DROP DATABASE IF EXISTS otaria_ex; DROP DATABASE IF EXISTS otaria_im; DROP DATABASE IF EXISTS otaria_cp; CREATE DATABASE otaria_ex; CREATE DATABASE otaria_im;
             CREATE TABLE otaria_ex.t (id INT PRIMARY KEY, txt TEXT, bin VARBINARY(8), d DECIMAL(30,2), ts TIMESTAMP NULL,
               g INT AS (id * 2) VIRTUAL, inv INT INVISIBLE DEFAULT 5) WITH SYSTEM VERSIONING;
             INSERT INTO otaria_ex.t (id, txt, bin, d, ts, inv) VALUES (1, 'a''b\\\\c\\n;', 0x00FF, 12345678901234567890.25, '2026-01-02 03:04:05', 9),
               (2, NULL, NULL, NULL, NULL, 5);
             CREATE SEQUENCE otaria_ex.s START WITH 10 NOCACHE; SELECT NEXTVAL(otaria_ex.s);
             CREATE VIEW otaria_ex.v AS SELECT id FROM otaria_ex.t;
             CREATE VIEW otaria_ex.a_top AS SELECT id FROM otaria_ex.v;
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
            null_as_n: false,
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
        // v, a_top (reads v, sorts before it), p, tr.
        assert_eq!(objs, Some(4));

        // Rows → CSV / JSON / Excel.
        assert_eq!(export(&pool, &req(Format::Csv, Some("t"), "csv"), &no, |_| {}).await.unwrap(), 2);
        let csv = std::fs::read_to_string(path("csv")).unwrap();
        assert!(csv.starts_with("id,txt,bin,d,ts,g,inv\n1,\"a'b\\c\n;\","), "{csv}");
        assert!(csv.ends_with("\n2,,,,,4,5\n"), "{csv}");
        // NULL as \N on request (the CSV import below reads \N back as NULL).
        let with_n = ExportReq { null_as_n: true, ..req(Format::Csv, Some("t"), "n.csv") };
        export(&pool, &with_n, &no, |_| {}).await.unwrap();
        let csv = std::fs::read_to_string(path("n.csv")).unwrap();
        assert!(csv.ends_with("\n2,\\N,\\N,\\N,\\N,4,5\n"), "{csv}");
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

        // CSV → table: BOM, quoted delimiter and newline, empty = NULL, \N = NULL, unmapped column skipped.
        conn.query_drop("CREATE TABLE otaria_im.c (id INT PRIMARY KEY, name VARCHAR(20), n INT)").await.unwrap();
        std::fs::write(path("in.csv"), "\u{feff}id;skip;name;n\n1;x;\"a;\nb\";5\n2;x;;\n3;x;\\N;7\n").unwrap();
        let pre = csv_preview(&path("in.csv"), None, true).unwrap();
        assert_eq!((pre.delimiter.as_str(), pre.columns[0].as_str(), pre.rows.len()), (";", "id", 3));
        let imp = |mapping: Vec<Option<&str>>| CsvImport {
            schema: "otaria_im".into(),
            table: "c".into(),
            path: path("in.csv"),
            delimiter: ";".into(),
            header: true,
            mapping: mapping.into_iter().map(|m| m.map(Into::into)).collect(),
            empty_null: true,
        };
        assert_eq!(import_csv(&pool, &imp(vec![Some("id"), None, Some("name"), Some("n")]), &no, |_, _| {}).await.unwrap(), 3);
        let got: Vec<(i32, Option<String>, Option<i32>)> = conn.query("SELECT id, name, n FROM otaria_im.c ORDER BY id").await.unwrap();
        assert_eq!(got, [(1, Some("a;\nb".into()), Some(5)), (2, None, None), (3, None, Some(7))]);
        // Same file again: duplicate keys, nothing kept, lines of the failing batch reported.
        conn.query_drop("DELETE FROM otaria_im.c WHERE id = 2").await.unwrap();
        let err = import_csv(&pool, &imp(vec![Some("id"), None, Some("name"), Some("n")]), &no, |_, _| {}).await.unwrap_err();
        assert_eq!((err.code, err.detail.as_deref()), ("IMPORT_ROWS", Some("2-5")));
        let left: Option<u64> = conn.query_first("SELECT COUNT(*) FROM otaria_im.c").await.unwrap();
        assert_eq!(left, Some(2));
        assert_eq!(import_csv(&pool, &imp(vec![Some("nope")]), &no, |_, _| {}).await.unwrap_err().code, "NOT_FOUND");

        // Copy one table into a schema that does not exist yet.
        let req = CopyReq { schema: "otaria_ex".into(), table: Some("t".into()), target_session: String::new(), target_schema: "otaria_cp".into(), data: true, drop: false };
        assert_eq!(copy(&pool, &pool, &req, &no, |_, _| {}).await.unwrap(), 2);
        let copied: Option<u64> = conn.query_first("SELECT COUNT(*) FROM otaria_cp.t").await.unwrap();
        assert_eq!(copied, Some(2));

        conn.query_drop("DROP DATABASE otaria_ex; DROP DATABASE otaria_im; DROP DATABASE otaria_cp").await.unwrap();
    }
}
