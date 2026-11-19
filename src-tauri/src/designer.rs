//! Table designer: load a table definition, diff two definitions into one ALTER TABLE, CREATE TABLE,
//! the table-level operations (rename, truncate, drop), and CREATE DATABASE.
//!
//! DDL cannot take parameters. Identifiers go through `ident`, text values (defaults, comments) through
//! `literal`, keywords (engine, collation, FK rules, index kinds) are checked against a whitelist.
//! Types and expressions (generated columns, DEFAULT expressions, CHECK clauses) are the user's own SQL,
//! run with their own privileges, like the grid filter or the SQL editor.

use std::collections::HashMap;
use mysql_async::prelude::Queryable;
use mysql_async::{Pool, Row};
use serde::{Deserialize, Serialize};

use crate::browse::ident;
use crate::versioning::Period;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum DefaultValue {
    None,
    Null,
    /// A text value, written as a quoted literal.
    Literal(String),
    /// SQL expression, written as-is (current_timestamp(), 0, uuid()…).
    Expr(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Generated {
    pub expr: String,
    pub stored: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDef {
    /// Name as loaded; None = new column. Lets the diff tell a rename from drop + add.
    pub original: Option<String>,
    pub name: String,
    pub col_type: String,
    pub nullable: bool,
    pub default: DefaultValue,
    pub generated: Option<Generated>,
    pub auto_increment: bool,
    pub on_update: Option<String>,
    pub invisible: bool,
    /// Only when it differs from the table's default collation.
    pub collation: Option<String>,
    /// Column-level CHECK (MariaDB; e.g. json_valid() on JSON columns): restated on every CHANGE.
    pub check: Option<String>,
    pub comment: String,
    /// WITHOUT SYSTEM VERSIONING: changes to it leave no history (versioned tables only).
    #[serde(default)]
    pub unversioned: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexColumn {
    pub name: String,
    /// Prefix length, e.g. 10 for `name`(10).
    pub length: Option<u32>,
    /// Descending order (MariaDB 10.8+, MySQL 8).
    #[serde(default)]
    pub desc: bool,
    /// Functional key part (MySQL 8): the expression, written `(expr)` in place of the column; `name` is then empty.
    #[serde(default)]
    pub expr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexDef {
    pub original: Option<String>,
    /// "PRIMARY" for the primary key; empty = let the server name it.
    pub name: String,
    /// PRIMARY | UNIQUE | INDEX | FULLTEXT | SPATIAL | VECTOR (MariaDB 11.7+)
    pub kind: String,
    pub columns: Vec<IndexColumn>,
    pub comment: String,
    /// Not used by the optimizer: IGNORED (MariaDB 10.6+) / INVISIBLE (MySQL 8).
    #[serde(default)]
    pub ignored: bool,
    /// VECTOR only: graph neighbours (3–200) and metric (euclidean | cosine); None = server default.
    #[serde(default)]
    pub m: Option<u32>,
    #[serde(default)]
    pub distance: Option<String>,
    /// `period WITHOUT OVERLAPS` after the columns (PRIMARY / UNIQUE on an application period).
    #[serde(default)]
    pub overlaps: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FkDef {
    pub original: Option<String>,
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_update: String,
    pub on_delete: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckDef {
    pub original: Option<String>,
    pub name: String,
    pub clause: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDef {
    pub name: String,
    pub engine: String,
    pub collation: String,
    pub comment: String,
    pub columns: Vec<ColumnDef>,
    pub indexes: Vec<IndexDef>,
    pub fks: Vec<FkDef>,
    pub checks: Vec<CheckDef>,
    /// WITH SYSTEM VERSIONING (MariaDB).
    #[serde(default)]
    pub versioned: bool,
    /// `PERIOD FOR name (start, end)`: application periods, and SYSTEM_TIME when its ROW START /
    /// ROW END columns are explicit (implicit ones are not listed).
    #[serde(default)]
    pub periods: Vec<Period>,
    /// `PARTITION BY …` clause as SHOW CREATE TABLE writes it; empty = not partitioned. Edited as SQL.
    #[serde(default)]
    pub partitions: String,
}

/// MySQL's STATISTICS.EXPRESSION is the SHOW CREATE TABLE text escaped once more (`\'` for `'`,
/// `\\` for `\`): one level of backslashes removed gives back SQL that runs (between `ESCAPES_ON` and
/// `ESCAPES_RESTORE`, like every designer statement).
fn unescape_once(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' { chars.next().unwrap_or('\\') } else { c });
    }
    out
}

/// Quoted text literal. `'` is doubled and `\` doubled: the statements run between `ESCAPES_ON` and
/// `ESCAPES_RESTORE`, so the backslash escape holds even on a server set to NO_BACKSLASH_ESCAPES.
pub fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
}

/// Turns NO_BACKSLASH_ESCAPES off for the session (saving its sql_mode), so `literal`'s escapes read as meant.
pub const ESCAPES_ON: &str = "SET @otaria_sql_mode = @@sql_mode, SESSION sql_mode = REPLACE(@@sql_mode, 'NO_BACKSLASH_ESCAPES', '')";
pub const ESCAPES_RESTORE: &str = "SET SESSION sql_mode = @otaria_sql_mode";

/// Engine and collation names: plain words only.
fn word(field: &'static str, s: &str) -> AppResult<String> {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(s.to_string())
    } else {
        Err(AppError { detail: Some(field.into()), ..AppError::new("INVALID_NAME", s.to_string()) })
    }
}

fn fk_rule(s: &str) -> AppResult<&'static str> {
    const RULES: [&str; 5] = ["RESTRICT", "CASCADE", "SET NULL", "NO ACTION", "SET DEFAULT"];
    RULES
        .into_iter()
        .find(|r| r.eq_ignore_ascii_case(s.trim()))
        .ok_or_else(|| AppError { detail: Some("fk_rule".into()), ..AppError::new("INVALID_NAME", s.to_string()) })
}

fn nonempty(field: &'static str, s: &str) -> AppResult<()> {
    match s.trim().is_empty() {
        true => Err(AppError { detail: Some(field.into()), ..AppError::new("REQUIRED", String::new()) }),
        false => Ok(()),
    }
}

pub fn column_sql(c: &ColumnDef) -> AppResult<String> {
    nonempty("column_name", &c.name)?;
    nonempty("column_type", &c.col_type)?;
    let mut s = format!("{} {}", ident(&c.name), c.col_type.trim());
    if let Some(coll) = c.collation.as_deref().filter(|s| !s.is_empty()) {
        s += &format!(" COLLATE {}", word("collation", coll)?);
    }
    match &c.generated {
        // ROW START / ROW END of a system-versioned table with explicit period columns.
        Some(g) if is_row_period(&g.expr) => s += &format!(" GENERATED ALWAYS AS {}", g.expr.trim().to_ascii_uppercase()),
        Some(g) => {
            nonempty("generated", &g.expr)?;
            s += &format!(" AS ({}) {}", g.expr.trim(), if g.stored { "PERSISTENT" } else { "VIRTUAL" });
        }
        None => {
            s += if c.nullable { " NULL" } else { " NOT NULL" };
            match &c.default {
                DefaultValue::None => {}
                DefaultValue::Null => s += " DEFAULT NULL",
                DefaultValue::Literal(v) => s += &format!(" DEFAULT {}", literal(v)),
                DefaultValue::Expr(e) => s += &format!(" DEFAULT ({})", e.trim()),
            }
            if c.auto_increment {
                s += " AUTO_INCREMENT";
            }
            if let Some(u) = c.on_update.as_deref().filter(|u| !u.trim().is_empty()) {
                s += &format!(" ON UPDATE {}", u.trim());
            }
        }
    }
    if c.invisible {
        s += " INVISIBLE";
    }
    if c.unversioned {
        s += " WITHOUT SYSTEM VERSIONING";
    }
    if !c.comment.is_empty() {
        s += &format!(" COMMENT {}", literal(&c.comment));
    }
    if let Some(chk) = c.check.as_deref().filter(|s| !s.trim().is_empty()) {
        s += &format!(" CHECK ({})", chk.trim());
    }
    Ok(s)
}

fn is_row_period(expr: &str) -> bool {
    let e = expr.trim();
    e.eq_ignore_ascii_case("ROW START") || e.eq_ignore_ascii_case("ROW END")
}

fn period_sql(p: &Period) -> AppResult<String> {
    nonempty("period", &p.name)?;
    let name = if p.name == "SYSTEM_TIME" { "SYSTEM_TIME".to_string() } else { ident(p.name.trim()) };
    Ok(format!("PERIOD FOR {name} ({}, {})", ident(&p.start), ident(&p.end)))
}

/// `mariadb` picks the keyword of an index the optimizer must not use: IGNORED, or INVISIBLE on MySQL.
fn ignored_kw(ignored: bool, mariadb: bool) -> &'static str {
    match (ignored, mariadb) {
        (true, true) => "IGNORED",
        (false, true) => "NOT IGNORED",
        (true, false) => "INVISIBLE",
        (false, false) => "VISIBLE",
    }
}

fn index_sql(i: &IndexDef, mariadb: bool) -> AppResult<String> {
    if i.columns.is_empty() {
        return Err(AppError { detail: Some("index_columns".into()), ..AppError::new("REQUIRED", i.name.clone()) });
    }
    let cols: Vec<String> = i
        .columns
        .iter()
        .map(|c| {
            let col = match (&c.expr, c.length) {
                (Some(e), _) => format!("({})", e.trim()),
                (None, Some(n)) => format!("{}({n})", ident(&c.name)),
                (None, None) => ident(&c.name),
            };
            if c.desc { col + " DESC" } else { col }
        })
        .collect();
    let mut cols = cols;
    if let Some(p) = i.overlaps.as_deref().filter(|p| !p.is_empty()) {
        cols.push(format!("{} WITHOUT OVERLAPS", ident(p)));
    }
    let name = if i.name.trim().is_empty() { String::new() } else { format!("{} ", ident(i.name.trim())) };
    let mut s = match i.kind.as_str() {
        "PRIMARY" => format!("PRIMARY KEY ({})", cols.join(", ")),
        "UNIQUE" | "FULLTEXT" | "SPATIAL" | "VECTOR" => format!("{} INDEX {name}({})", i.kind, cols.join(", ")),
        "INDEX" => format!("INDEX {name}({})", cols.join(", ")),
        other => return Err(AppError { detail: Some("index_kind".into()), ..AppError::new("INVALID_NAME", other.to_string()) }),
    };
    if i.kind == "VECTOR" {
        if let Some(m) = i.m {
            if !(3..=200).contains(&m) {
                return Err(AppError { detail: Some("vector_m".into()), ..AppError::new("INVALID_NAME", m.to_string()) });
            }
            s += &format!(" M={m}");
        }
        match i.distance.as_deref() {
            None => {}
            Some(d @ ("euclidean" | "cosine")) => s += &format!(" DISTANCE={d}"),
            Some(d) => return Err(AppError { detail: Some("vector_distance".into()), ..AppError::new("INVALID_NAME", d.to_string()) }),
        }
    }
    if !i.comment.is_empty() {
        s += &format!(" COMMENT {}", literal(&i.comment));
    }
    if i.ignored {
        s += &format!(" {}", ignored_kw(true, mariadb));
    }
    Ok(s)
}

fn fk_sql(f: &FkDef) -> AppResult<String> {
    nonempty("fk_table", &f.ref_table)?;
    if f.columns.is_empty() || f.columns.len() != f.ref_columns.len() {
        return Err(AppError { detail: Some("fk_columns".into()), ..AppError::new("REQUIRED", f.name.clone()) });
    }
    let list = |v: &[String]| v.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ");
    let name = if f.name.trim().is_empty() { String::new() } else { format!("CONSTRAINT {} ", ident(f.name.trim())) };
    let target = match f.ref_schema.trim() {
        "" => ident(&f.ref_table),
        s => format!("{}.{}", ident(s), ident(&f.ref_table)),
    };
    Ok(format!(
        "{name}FOREIGN KEY ({}) REFERENCES {target} ({}) ON DELETE {} ON UPDATE {}",
        list(&f.columns),
        list(&f.ref_columns),
        fk_rule(&f.on_delete)?,
        fk_rule(&f.on_update)?
    ))
}

fn check_sql(c: &CheckDef) -> AppResult<String> {
    nonempty("check", &c.clause)?;
    let name = if c.name.trim().is_empty() { String::new() } else { format!("CONSTRAINT {} ", ident(c.name.trim())) };
    Ok(format!("{name}CHECK ({})", c.clause.trim()))
}

fn table_options(t: &TableDef) -> AppResult<Vec<String>> {
    let mut o = Vec::new();
    if !t.engine.is_empty() {
        o.push(format!("ENGINE={}", word("engine", &t.engine)?));
    }
    if !t.collation.is_empty() {
        o.push(format!("COLLATE={}", word("collation", &t.collation)?));
    }
    o.push(format!("COMMENT={}", literal(&t.comment)));
    if t.versioned {
        o.push("WITH SYSTEM VERSIONING".into());
    }
    Ok(o)
}

pub fn create_sql(schema: &str, t: &TableDef, mariadb: bool) -> AppResult<String> {
    nonempty("table_name", &t.name)?;
    if t.columns.is_empty() {
        return Err(AppError { detail: Some("columns".into()), ..AppError::new("REQUIRED", String::new()) });
    }
    let mut parts: Vec<String> = t.columns.iter().map(column_sql).collect::<AppResult<_>>()?;
    for p in &t.periods {
        parts.push(period_sql(p)?);
    }
    for i in &t.indexes {
        parts.push(index_sql(i, mariadb)?);
    }
    for f in &t.fks {
        parts.push(fk_sql(f)?);
    }
    for c in &t.checks {
        parts.push(check_sql(c)?);
    }
    let part = if t.partitions.trim().is_empty() { String::new() } else { format!("\n{}", t.partitions.trim()) };
    Ok(format!(
        "CREATE TABLE {}.{} (\n  {}\n) {}{part}",
        ident(schema),
        ident(&t.name),
        parts.join(",\n  "),
        table_options(t)?.join(" ")
    ))
}

/// How the ALTER may run. The UI tries Instant first, then Online, then Copy: with an explicit
/// ALGORITHM the server refuses (ALTER_ALGORITHM) instead of silently rebuilding the table.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Algorithm {
    Instant,
    /// INPLACE, LOCK=NONE: rebuilds but keeps the table readable and writable.
    Online,
    /// Server's choice (may copy the table and block writes).
    Default,
}

/// Everything that changed between `old` and `new`, as the clauses of one ALTER TABLE.
pub fn alter_clauses(old: &TableDef, new: &TableDef, mariadb: bool) -> AppResult<Vec<String>> {
    let mut out = Vec::new();
    let kept = |orig: &Option<String>| orig.clone();

    // 1. Drops: constraints and indexes first (they may use dropped columns), then columns.
    for f in &old.fks {
        if new.fks.iter().find(|n| n.original.as_deref() == Some(&f.name)).is_none_or(|n| strip_fk(n) != strip_fk(f)) {
            out.push(format!("DROP FOREIGN KEY {}", ident(&f.name)));
        }
    }
    for c in &old.checks {
        if new.checks.iter().find(|n| n.original.as_deref() == Some(&c.name)).is_none_or(|n| n.name != c.name || n.clause != c.clause) {
            out.push(format!("DROP CONSTRAINT {}", ident(&c.name)));
        }
    }
    for i in &old.indexes {
        if new.indexes.iter().find(|n| n.original.as_deref() == Some(&i.name)).is_none_or(|n| strip_index(n) != strip_index(i)) {
            out.push(if i.kind == "PRIMARY" { "DROP PRIMARY KEY".into() } else { format!("DROP INDEX {}", ident(&i.name)) });
        }
    }
    // SYSTEM_TIME goes with DROP SYSTEM VERSIONING (and its columns), not on its own.
    for p in old.periods.iter().filter(|p| p.name != "SYSTEM_TIME" && !new.periods.contains(p)) {
        out.push(format!("DROP PERIOD FOR {}", ident(&p.name)));
    }
    for c in &old.columns {
        if !new.columns.iter().any(|n| n.original.as_deref() == Some(&c.name)) {
            out.push(format!("DROP COLUMN {}", ident(&c.name)));
        }
    }

    // 2. Columns, in their new order. A kept column moves when its kept predecessor changed.
    // ponytail: swapping two columns repositions both (one would do); harmless, just a longer statement.
    let old_kept: Vec<&str> = old
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .filter(|n| new.columns.iter().any(|c| c.original.as_deref() == Some(*n)))
        .collect();
    let new_kept: Vec<&str> = new.columns.iter().filter_map(|c| c.original.as_deref()).collect();
    for (i, c) in new.columns.iter().enumerate() {
        let pos = match i {
            0 => " FIRST".to_string(),
            _ => format!(" AFTER {}", ident(&new.columns[i - 1].name)),
        };
        match kept(&c.original) {
            None => out.push(format!("ADD COLUMN {}{pos}", column_sql(c)?)),
            Some(orig) => {
                let before = old.columns.iter().find(|o| o.name == orig).ok_or_else(|| AppError::new("UNKNOWN", orig.clone()))?;
                let j = new_kept.iter().position(|n| *n == orig).unwrap_or(0);
                let k = old_kept.iter().position(|n| *n == orig).unwrap_or(0);
                let moved = (j == 0) != (k == 0) || (j > 0 && k > 0 && new_kept[j - 1] != old_kept[k - 1]);
                let changed = ColumnDef { original: None, ..c.clone() } != ColumnDef { original: None, ..before.clone() };
                if changed || moved {
                    out.push(format!("CHANGE COLUMN {} {}{}", ident(&orig), column_sql(c)?, if moved { pos.as_str() } else { "" }));
                }
            }
        }
    }

    // 3. Adds: periods first, an index may use one WITHOUT OVERLAPS.
    for p in new.periods.iter().filter(|p| !old.periods.contains(p)) {
        out.push(format!("ADD {}", period_sql(p)?));
    }
    for i in &new.indexes {
        match old.indexes.iter().find(|o| i.original.as_deref() == Some(&o.name)) {
            Some(o) if strip_index(o) == strip_index(i) => {
                // Only the ignored flag changed: switched in place, no rebuild.
                if o.ignored != i.ignored {
                    out.push(format!("ALTER INDEX {} {}", ident(&i.name), ignored_kw(i.ignored, mariadb)));
                }
            }
            _ => out.push(format!("ADD {}", index_sql(i, mariadb)?)),
        }
    }
    for f in &new.fks {
        if old.fks.iter().find(|o| f.original.as_deref() == Some(&o.name)).is_none_or(|o| strip_fk(o) != strip_fk(f)) {
            out.push(format!("ADD {}", fk_sql(f)?));
        }
    }
    for c in &new.checks {
        if old.checks.iter().find(|o| c.original.as_deref() == Some(&o.name)).is_none_or(|o| o.name != c.name || o.clause != c.clause) {
            out.push(format!("ADD {}", check_sql(c)?));
        }
    }

    // 4. Table options.
    if new.engine != old.engine {
        out.push(format!("ENGINE={}", word("engine", &new.engine)?));
    }
    if new.collation != old.collation {
        out.push(format!("COLLATE={}", word("collation", &new.collation)?));
    }
    if new.comment != old.comment {
        out.push(format!("COMMENT={}", literal(&new.comment)));
    }
    match (old.versioned, new.versioned) {
        (false, true) => out.push("ADD SYSTEM VERSIONING".into()),
        (true, false) => out.push("DROP SYSTEM VERSIONING".into()),
        _ => {}
    }
    Ok(out)
}

/// Index without what does not need a rebuild (its loaded name, the ignored flag).
fn strip_index(i: &IndexDef) -> IndexDef {
    IndexDef { original: None, ignored: false, ..i.clone() }
}

fn strip_fk(f: &FkDef) -> FkDef {
    FkDef { original: None, ..f.clone() }
}

/// The full statements to run: RENAME first (when the name changed), then one ALTER TABLE.
pub fn alter_sql(schema: &str, old: &TableDef, new: &TableDef, algo: Algorithm, mariadb: bool) -> AppResult<Vec<String>> {
    if !old.versioned && new.versioned && new.columns.iter().any(|c| c.unversioned) {
        // WITHOUT SYSTEM VERSIONING is only accepted once the table is versioned: second ALTER.
        let mut mid = new.clone();
        mid.columns.iter_mut().for_each(|c| c.unversioned = false);
        let mut out = alter_sql(schema, old, &mid, algo, mariadb)?;
        let cols: Vec<String> =
            new.columns.iter().filter(|c| c.unversioned).map(|c| Ok(format!("CHANGE COLUMN {} {}", ident(&c.name), column_sql(c)?))).collect::<AppResult<_>>()?;
        out.push("SET SESSION system_versioning_alter_history = KEEP".into());
        out.push(format!("ALTER TABLE {}.{}\n  {}", ident(schema), ident(new.name.trim()), cols.join(",\n  ")));
        return Ok(out);
    }
    let mut out = Vec::new();
    if new.name.trim() != old.name {
        nonempty("table_name", &new.name)?;
        out.push(rename_sql(schema, &old.name, new.name.trim()));
    }
    let clauses = alter_clauses(old, new, mariadb)?;
    // Partitioning is not a comma-separated clause: its own ALTER, after the others.
    let part = (new.partitions.trim() != old.partitions.trim()).then(|| match new.partitions.trim() {
        "" => format!("ALTER TABLE {}.{} REMOVE PARTITIONING", ident(schema), ident(new.name.trim())),
        p => format!("ALTER TABLE {}.{}\n  {p}", ident(schema), ident(new.name.trim())),
    });
    if (!clauses.is_empty() || part.is_some()) && old.versioned {
        // Without it the server refuses any ALTER of a system-versioned table (error 4119).
        // Session only: the pool resets the connection when it is given back.
        out.push("SET SESSION system_versioning_alter_history = KEEP".into());
    }
    if !clauses.is_empty() {
        let algo = match algo {
            Algorithm::Instant => ", ALGORITHM=INSTANT",
            Algorithm::Online => ", ALGORITHM=INPLACE, LOCK=NONE",
            Algorithm::Default => "",
        };
        out.push(format!("ALTER TABLE {}.{}\n  {}{algo}", ident(schema), ident(new.name.trim()), clauses.join(",\n  ")));
    }
    out.extend(part);
    Ok(out)
}

pub fn rename_sql(schema: &str, from: &str, to: &str) -> String {
    format!("RENAME TABLE {}.{} TO {}.{}", ident(schema), ident(from), ident(schema), ident(to))
}

/// DROP / TRUNCATE for the object kinds of the tree.
pub fn drop_sql(schema: &str, name: &str, kind: &str, truncate: bool) -> AppResult<String> {
    let keyword = match kind {
        "table" => "TABLE",
        "view" => "VIEW",
        "sequence" => "SEQUENCE",
        "procedure" => "PROCEDURE",
        "function" => "FUNCTION",
        "trigger" => "TRIGGER",
        "event" => "EVENT",
        _ => return Err(AppError::new("UNKNOWN", format!("kind {kind}"))),
    };
    let target = format!("{}.{}", ident(schema), ident(name));
    Ok(match (truncate, kind) {
        (true, "table") => format!("TRUNCATE TABLE {target}"),
        (true, _) => return Err(AppError::new("UNKNOWN", format!("truncate {kind}"))),
        _ => format!("DROP {keyword} {target}"),
    })
}

/// Run statements one by one (DDL commits on its own: no transaction is possible).
/// The pooled connection gets its sql_mode back, failure or not.
pub async fn run(pool: &Pool, stmts: &[String]) -> AppResult<()> {
    run_on(&mut pool.get_conn().await?, stmts).await
}

/// `run` on a given connection: the caller knows its id (progress, KILL QUERY).
pub async fn run_on(conn: &mut mysql_async::Conn, stmts: &[String]) -> AppResult<()> {
    conn.query_drop(ESCAPES_ON).await?;
    let mut result = Ok(());
    for s in stmts {
        if let Err(e) = conn.query_drop(s).await {
            result = Err(e.into());
            break;
        }
    }
    conn.query_drop(ESCAPES_RESTORE).await?;
    result
}

/// MariaDB: how long the ALTERs of `table` wait for the metadata lock. None = server default
/// (lock_wait_timeout), Some(0) = NOWAIT (fail at once), Some(n) = WAIT n seconds.
pub fn with_wait(stmts: Vec<String>, schema: &str, table: &str, wait: Option<u32>) -> Vec<String> {
    let Some(n) = wait else { return stmts };
    let head = format!("ALTER TABLE {}.{}", ident(schema), ident(table.trim()));
    let kw = if n == 0 { " NOWAIT".to_string() } else { format!(" WAIT {n}") };
    stmts.into_iter().map(|s| if s.starts_with(&head) { format!("{head}{kw}{}", &s[head.len()..]) } else { s }).collect()
}

/// How far a running ALTER (or LOAD DATA…) of connection `id` is, MariaDB only: (percent, stage, stages).
pub async fn progress(pool: &Pool, id: u32) -> AppResult<Option<(f64, u32, u32)>> {
    Ok(pool
        .get_conn()
        .await?
        .exec_first("SELECT PROGRESS + 0e0, STAGE, MAX_STAGE FROM information_schema.PROCESSLIST WHERE ID = ?", (id,))
        .await?)
}

/// Parse information_schema.COLUMN_DEFAULT. MariaDB quotes literals ('abc'), writes 'NULL' for
/// DEFAULT NULL and leaves expressions bare. MySQL never quotes and flags expressions in EXTRA.
fn parse_default(raw: Option<String>, mariadb: bool, extra: &str) -> DefaultValue {
    match raw {
        None => DefaultValue::None,
        Some(s) if mariadb && s == "NULL" => DefaultValue::Null,
        Some(s) if mariadb && s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') => {
            DefaultValue::Literal(s[1..s.len() - 1].replace("''", "'").replace("\\\\", "\\"))
        }
        Some(s) if mariadb => DefaultValue::Expr(s),
        Some(s) if extra.contains("default_generated") => DefaultValue::Expr(s),
        Some(s) => DefaultValue::Literal(s),
    }
}

pub async fn load(pool: &Pool, schema: &str, table: &str, mariadb: bool) -> AppResult<TableDef> {
    load_tables(pool, schema, Some(table), mariadb).await?.pop().ok_or_else(|| AppError::new("NOT_FOUND", table.to_string()))
}

/// Rows of one information_schema query, grouped by the table name in their first column.
fn by_table<T>(rows: Vec<(String, T)>) -> HashMap<String, Vec<T>> {
    let mut map: HashMap<String, Vec<T>> = HashMap::new();
    for (table, r) in rows {
        map.entry(table).or_default().push(r);
    }
    map
}

/// Every table of `schema` (views and sequences left out), or only `only`: the same five queries
/// whatever the number of tables. SHOW CREATE TABLE is read only for a table that needs it (vector
/// index options, FK rules hidden from a SELECT-only account).
pub async fn load_tables(pool: &Pool, schema: &str, only: Option<&str>, mariadb: bool) -> AppResult<Vec<TableDef>> {
    let mut conn = pool.get_conn().await?;
    // `? IS NULL OR TABLE_NAME = ?`: all tables, or one.
    let which = (schema, only, only);
    let tables: Vec<(String, String, String, String, bool, bool)> = conn
        .exec(
            "SELECT TABLE_NAME, IFNULL(ENGINE, ''), IFNULL(TABLE_COLLATION, ''), IFNULL(TABLE_COMMENT, ''), TABLE_TYPE = 'SYSTEM VERSIONED',
                    IFNULL(CREATE_OPTIONS LIKE '%partitioned%', 0)
               FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND (? IS NULL OR TABLE_NAME = ?)
                AND TABLE_TYPE NOT IN ('VIEW', 'SYSTEM VIEW', 'SEQUENCE') ORDER BY TABLE_NAME",
            which,
        )
        .await?;

    // Column-level CHECKs (MariaDB only) are named after their column.
    let checks: Vec<(String, (String, String, String))> = if mariadb {
        let rows: Vec<(String, String, String, String)> = conn
            .exec(
                "SELECT TABLE_NAME, CONSTRAINT_NAME, CHECK_CLAUSE, LEVEL FROM information_schema.CHECK_CONSTRAINTS
                  WHERE CONSTRAINT_SCHEMA = ? AND (? IS NULL OR TABLE_NAME = ?) ORDER BY TABLE_NAME, CONSTRAINT_NAME",
                which,
            )
            .await?;
        rows.into_iter().map(|(t, n, c, l)| (t, (n, c, l))).collect()
    } else {
        // MySQL 8.0.16+; older servers have no such table: no checks then.
        let rows: Vec<(String, String, String)> = conn
            .exec(
                "SELECT t.TABLE_NAME, c.CONSTRAINT_NAME, c.CHECK_CLAUSE FROM information_schema.CHECK_CONSTRAINTS c
                   JOIN information_schema.TABLE_CONSTRAINTS t USING (CONSTRAINT_SCHEMA, CONSTRAINT_NAME)
                  WHERE t.TABLE_SCHEMA = ? AND (? IS NULL OR t.TABLE_NAME = ?) AND t.CONSTRAINT_TYPE = 'CHECK' ORDER BY 1, 2",
                which,
            )
            .await
            .unwrap_or_default();
        rows.into_iter().map(|(t, n, c)| (t, (n, c, "Table".to_string()))).collect()
    };
    let mut checks = by_table(checks);

    let rows: Vec<Row> = conn
        .exec(
            "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE = 'YES', COLUMN_DEFAULT, LOWER(EXTRA), COLLATION_NAME,
                    COLUMN_COMMENT, IFNULL(GENERATION_EXPRESSION, '')
               FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND (? IS NULL OR TABLE_NAME = ?)
              ORDER BY TABLE_NAME, ORDINAL_POSITION",
            which,
        )
        .await?;
    let mut columns = by_table(rows.into_iter().map(|mut r| (r.take::<String, _>(0).unwrap_or_default(), r)).collect());

    // IGNORED exists from MariaDB 10.6 (oldest supported: 10.11); MySQL calls it IS_VISIBLE.
    let ignored = if mariadb { "IGNORED = 'YES'" } else { "IS_VISIBLE = 'NO'" };
    // Functional key parts: MySQL 8 lists their expression (COLUMN_NAME NULL); MariaDB has none.
    let expression = if mariadb { "NULL" } else { "EXPRESSION" };
    type IndexRow = (String, String, bool, Option<String>, Option<u32>, String, String, bool, bool, Option<String>);
    let rows: Vec<IndexRow> = conn
        .exec(
            format!(
                "SELECT TABLE_NAME, INDEX_NAME, NON_UNIQUE = 1, COLUMN_NAME, SUB_PART, INDEX_TYPE, IFNULL(INDEX_COMMENT, ''),
                        IFNULL(COLLATION = 'D', 0), {ignored}, {expression}
                   FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND (? IS NULL OR TABLE_NAME = ?)
                  ORDER BY TABLE_NAME, INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX"
            ),
            which,
        )
        .await?;
    let mut indexes = by_table(rows.into_iter().map(|(t, a, b, c, d, e, f, g, h, x)| (t, (a, b, c, d, e, f, g, h, x))).collect());

    // LEFT JOIN: with SELECT only, MariaDB lists the FK in KEY_COLUMN_USAGE but hides its
    // REFERENTIAL_CONSTRAINTS row; the rules then come from SHOW CREATE TABLE (visible with SELECT).
    type FkRow = (String, String, String, String, String, String, Option<String>, Option<String>);
    let rows: Vec<FkRow> = conn
        .exec(
            "SELECT k.TABLE_NAME, k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME,
                    k.REFERENCED_COLUMN_NAME, r.UPDATE_RULE, r.DELETE_RULE
               FROM information_schema.KEY_COLUMN_USAGE k
               LEFT JOIN information_schema.REFERENTIAL_CONSTRAINTS r
                 ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME
              WHERE k.TABLE_SCHEMA = ? AND (? IS NULL OR k.TABLE_NAME = ?) AND k.REFERENCED_TABLE_NAME IS NOT NULL
              ORDER BY k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
            which,
        )
        .await?;
    let mut fk_rows = by_table(rows.into_iter().map(|(t, a, b, c, d, e, f, g)| (t, (a, b, c, d, e, f, g))).collect());

    let mut out = Vec::new();
    for (table, engine, collation, comment, versioned, partitioned) in tables {
        let mut col_checks: Vec<(String, String)> = Vec::new();
        let mut table_checks = Vec::new();
        for (name, clause, level) in checks.remove(&table).unwrap_or_default() {
            match level.as_str() {
                "Column" => col_checks.push((name, clause)),
                _ => table_checks.push(CheckDef { original: Some(name.clone()), name, clause }),
            }
        }

        let mut cols = Vec::new();
        for mut r in columns.remove(&table).unwrap_or_default() {
            let name: String = r.take(1).unwrap_or_default();
            let extra: String = r.take(5).unwrap_or_default();
            let coll: Option<String> = r.take(6).unwrap_or_default();
            let gen_expr: String = r.take(8).unwrap_or_default();
            let generated = (extra.contains("virtual generated") || extra.contains("stored generated"))
                .then(|| Generated { expr: gen_expr, stored: extra.contains("stored") });
            let on_update = extra.find("on update ").map(|i| {
                let rest = &extra[i + 10..];
                rest.split_whitespace().next().unwrap_or_default().to_string()
            });
            cols.push(ColumnDef {
                original: Some(name.clone()),
                check: col_checks.iter().find(|(n, _)| *n == name).map(|(_, c)| c.clone()),
                col_type: r.take(2).unwrap_or_default(),
                nullable: r.take(3).unwrap_or_default(),
                default: if generated.is_some() { DefaultValue::None } else { parse_default(r.take(4).unwrap_or_default(), mariadb, &extra) },
                auto_increment: extra.contains("auto_increment"),
                on_update,
                invisible: extra.contains("invisible"),
                collation: coll.filter(|c| *c != collation),
                comment: r.take(7).unwrap_or_default(),
                unversioned: extra.contains("without system versioning"),
                generated,
                name,
            });
        }

        let mut idx: Vec<IndexDef> = Vec::new();
        for (name, non_unique, col, sub_part, itype, icomment, desc, ignored, expr) in indexes.remove(&table).unwrap_or_default() {
            let column = match (col, expr) {
                (Some(col), _) => IndexColumn { name: col, length: sub_part, desc, expr: None },
                (None, Some(e)) => IndexColumn { name: String::new(), length: None, desc, expr: Some(unescape_once(&e)) },
                (None, None) => continue,
            };
            match idx.last_mut().filter(|i| i.name == name) {
                Some(i) => i.columns.push(column),
                None => idx.push(IndexDef {
                    original: Some(name.clone()),
                    kind: match (name.as_str(), itype.as_str(), non_unique) {
                        ("PRIMARY", _, _) => "PRIMARY",
                        (_, "FULLTEXT", _) => "FULLTEXT",
                        (_, "SPATIAL", _) => "SPATIAL",
                        (_, "VECTOR", _) => "VECTOR",
                        (_, _, false) => "UNIQUE",
                        _ => "INDEX",
                    }
                    .into(),
                    name,
                    columns: vec![column],
                    comment: icomment,
                    ignored,
                    m: None,
                    distance: None,
                    overlaps: None,
                }),
            }
        }

        let fk_list = fk_rows.remove(&table).unwrap_or_default();
        // SHOW CREATE TABLE only when needed: M and DISTANCE of a vector index (else a rebuilt one
        // could change), FK rules hidden from a SELECT-only account.
        // Also periods: a versioned table, or a CHECK (an application period comes with one, named after it).
        let needs_ddl = idx.iter().any(|i| i.kind == "VECTOR") || fk_list.iter().any(|r| r.5.is_none()) || versioned || partitioned || !table_checks.is_empty();
        let ddl = if needs_ddl { crate::browse::ddl_on(&mut conn, schema, &table, "table").await? } else { String::new() };
        if idx.iter().any(|i| i.kind == "VECTOR") {
            let found = crate::vector::vector_indexes(&ddl);
            for i in idx.iter_mut().filter(|i| i.kind == "VECTOR") {
                if let Some((_, v)) = found.iter().find(|(_, v)| v.name == i.name) {
                    (i.m, i.distance) = (v.m, v.distance.clone());
                }
            }
        }

        let mut fks: Vec<FkDef> = Vec::new();
        for (name, col, rs, rt, rc, upd, del) in fk_list {
            let (upd, del) = match (upd, del) {
                (Some(u), Some(d)) => (u, d),
                _ => fk_rules_from_ddl(&ddl, &name),
            };
            match fks.last_mut().filter(|f| f.name == name) {
                Some(f) => {
                    f.columns.push(col);
                    f.ref_columns.push(rc);
                }
                None => fks.push(FkDef {
                    original: Some(name.clone()),
                    name,
                    columns: vec![col],
                    // Same schema: leave empty so the FK follows the table if the schema is copied.
                    ref_schema: if rs == schema { String::new() } else { rs },
                    ref_table: rt,
                    ref_columns: vec![rc],
                    on_update: upd,
                    on_delete: del,
                }),
            }
        }

        let periods = crate::versioning::periods(&ddl);
        server_added_out(&periods, &mut idx, &mut table_checks);
        let partitions = partition_clause(&ddl);
        out.push(TableDef { name: table, engine, collation, comment, columns: cols, indexes: idx, fks, checks: table_checks, versioned, periods, partitions });
    }
    Ok(out)
}

/// The `PARTITION BY …` end of a SHOW CREATE TABLE (MySQL wraps it in `/*!50100 … */`); "" when none.
pub fn partition_clause(ddl: &str) -> String {
    let Some(i) = ddl.find("\n PARTITION BY").or_else(|| ddl.find("\n/*!50100 PARTITION BY")) else { return String::new() };
    let rest = ddl[i..].trim().trim_start_matches("/*!50100").trim();
    rest.strip_suffix("*/").unwrap_or(rest).trim().to_string()
}

/// What the server adds by itself, left out so the definition reads as written (and a CREATE from it
/// works): ROW END at the end of each unique key of a versioned table with explicit columns, the
/// `end, start` columns that stand for `period WITHOUT OVERLAPS`, the CHECK named after a period.
fn server_added_out(periods: &[Period], idx: &mut [IndexDef], checks: &mut Vec<CheckDef>) {
    for i in idx.iter_mut().filter(|i| i.kind == "PRIMARY" || i.kind == "UNIQUE") {
        for p in periods {
            let n = i.columns.len();
            if p.name == "SYSTEM_TIME" {
                if n > 1 && i.columns[n - 1].name == p.end {
                    i.columns.pop();
                }
            } else if n > 2 && i.columns[n - 2].name == p.end && i.columns[n - 1].name == p.start {
                i.columns.truncate(n - 2);
                i.overlaps = Some(p.name.clone());
            }
        }
    }
    checks.retain(|c| !periods.iter().any(|p| p.name == c.name));
}

/// ON UPDATE / ON DELETE of constraint `name` in SHOW CREATE TABLE; a missing clause is RESTRICT.
fn fk_rules_from_ddl(ddl: &str, name: &str) -> (String, String) {
    let line = ddl.lines().find(|l| l.contains(&format!("CONSTRAINT {} FOREIGN KEY", ident(name)))).unwrap_or("");
    let rule = |kw: &str| {
        line.split_once(kw).map_or("RESTRICT", |(_, rest)| {
            ["SET NULL", "SET DEFAULT", "NO ACTION", "CASCADE", "RESTRICT"].into_iter().find(|r| rest.trim_start().starts_with(r)).unwrap_or("RESTRICT")
        })
    };
    (rule("ON UPDATE").to_string(), rule("ON DELETE").to_string())
}

/// Engines the server can use, default first (for the designer's dropdown).
pub async fn engines(pool: &Pool) -> AppResult<Vec<String>> {
    let mut conn = pool.get_conn().await?;
    Ok(conn
        .query("SELECT ENGINE FROM information_schema.ENGINES WHERE SUPPORT IN ('YES', 'DEFAULT') ORDER BY SUPPORT = 'DEFAULT' DESC, ENGINE")
        .await?)
}

pub async fn collations(pool: &Pool) -> AppResult<Vec<String>> {
    let mut conn = pool.get_conn().await?;
    Ok(conn.query("SELECT COLLATION_NAME FROM information_schema.COLLATIONS ORDER BY 1").await?)
}

/// Character sets with their collations, and the server defaults to preselect (new database dialog).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Charsets {
    pub default_charset: String,
    pub default_collation: String,
    /// (charset, collations), the charset's default collation first.
    pub charsets: Vec<(String, Vec<String>)>,
}

pub async fn charsets(pool: &Pool) -> AppResult<Charsets> {
    let mut conn = pool.get_conn().await?;
    let (default_charset, default_collation): (String, String) =
        conn.query_first("SELECT @@character_set_server, @@collation_server").await?.unwrap_or_default();
    // MariaDB 10.10+ lists UCA 14 collations (the default since 11.x: utf8mb4_uca1400_ai_ci) without a charset in
    // COLLATIONS; their full per-charset names are only in FULL_COLLATION_NAME. MySQL has no such column.
    let full = "SELECT CHARACTER_SET_NAME, FULL_COLLATION_NAME FROM information_schema.COLLATION_CHARACTER_SET_APPLICABILITY
                ORDER BY CHARACTER_SET_NAME, IS_DEFAULT DESC, FULL_COLLATION_NAME";
    let plain = "SELECT CHARACTER_SET_NAME, COLLATION_NAME FROM information_schema.COLLATIONS
                 WHERE CHARACTER_SET_NAME IS NOT NULL ORDER BY CHARACTER_SET_NAME, IS_DEFAULT DESC, COLLATION_NAME";
    let rows: Vec<(String, String)> = match conn.query(full).await {
        Ok(rows) => rows,
        Err(_) => conn.query(plain).await?,
    };
    let mut charsets: Vec<(String, Vec<String>)> = Vec::new();
    for (cs, coll) in rows {
        match charsets.last_mut() {
            Some((last, colls)) if *last == cs => colls.push(coll),
            _ => charsets.push((cs, vec![coll])),
        }
    }
    Ok(Charsets { default_charset, default_collation, charsets })
}

/// Current default character set and collation of a database (edit dialog).
pub async fn database_options(pool: &Pool, name: &str) -> AppResult<(String, String)> {
    pool.get_conn()
        .await?
        .exec_first(
            "SELECT DEFAULT_CHARACTER_SET_NAME, DEFAULT_COLLATION_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?",
            (name,),
        )
        .await?
        .ok_or_else(|| AppError::new("NOT_FOUND", name.to_string()))
}

/// CREATE, ALTER or DROP DATABASE. Empty charset or collation = server default (CREATE) or unchanged (ALTER).
pub fn database_sql(action: &str, name: &str, charset: &str, collation: &str) -> AppResult<String> {
    if name.trim().is_empty() {
        return Err(AppError { detail: Some("name".into()), ..AppError::new("INVALID_NAME", String::new()) });
    }
    let verb = match action {
        "create" => "CREATE",
        "alter" => "ALTER",
        "drop" => return Ok(format!("DROP DATABASE {}", ident(name.trim()))),
        _ => return Err(AppError { detail: Some("action".into()), ..AppError::new("INVALID_NAME", action.to_string()) }),
    };
    let mut s = format!("{verb} DATABASE {}", ident(name.trim()));
    if !charset.is_empty() {
        s += &format!(" CHARACTER SET {}", word("charset", charset)?);
    }
    if !collation.is_empty() {
        s += &format!(" COLLATE {}", word("collation", collation)?);
    }
    if verb == "ALTER" && charset.is_empty() && collation.is_empty() {
        return Err(AppError { detail: Some("collation".into()), ..AppError::new("INVALID_NAME", String::new()) });
    }
    Ok(s)
}

pub async fn database_ddl(pool: &Pool, action: &str, name: &str, charset: &str, collation: &str) -> AppResult<()> {
    let sql = database_sql(action, name, charset, collation)?;
    Ok(pool.get_conn().await?.query_drop(sql).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_mysql_index_expressions() {
        assert_eq!(unescape_once(r"concat(`b`,_utf8mb4\'it\\\'s\',_utf8mb4\'b\\\\s\')"), r"concat(`b`,_utf8mb4'it\'s',_utf8mb4'b\\s')");
    }

    #[test]
    fn database_statements() {
        assert_eq!(database_sql("create", " shop ", "", "").unwrap(), "CREATE DATABASE `shop`");
        assert_eq!(
            database_sql("create", "a`b", "utf8mb4", "utf8mb4_uca1400_ai_ci").unwrap(),
            "CREATE DATABASE `a``b` CHARACTER SET utf8mb4 COLLATE utf8mb4_uca1400_ai_ci"
        );
        assert_eq!(database_sql("alter", "s", "latin1", "latin1_swedish_ci").unwrap(), "ALTER DATABASE `s` CHARACTER SET latin1 COLLATE latin1_swedish_ci");
        assert_eq!(database_sql("drop", "a`b", "ignored", "").unwrap(), "DROP DATABASE `a``b`");
        assert_eq!(database_sql("alter", "s", "", "").unwrap_err().code, "INVALID_NAME");
        assert_eq!(database_sql("create", "x", "utf8; DROP", "").unwrap_err().code, "INVALID_NAME");
        assert_eq!(database_sql("create", "  ", "", "").unwrap_err().code, "INVALID_NAME");
        assert_eq!(database_sql("truncate", "x", "", "").unwrap_err().code, "INVALID_NAME");
    }

    fn col(name: &str, ty: &str) -> ColumnDef {
        ColumnDef {
            original: Some(name.into()),
            name: name.into(),
            col_type: ty.into(),
            nullable: false,
            default: DefaultValue::None,
            generated: None,
            auto_increment: false,
            on_update: None,
            invisible: false,
            collation: None,
            check: None,
            comment: String::new(),
            unversioned: false,
        }
    }

    fn table() -> TableDef {
        TableDef {
            name: "t".into(),
            engine: "InnoDB".into(),
            collation: "utf8mb4_general_ci".into(),
            comment: String::new(),
            columns: vec![col("id", "int(11)"), col("a", "varchar(10)"), col("b", "int(11)")],
            indexes: vec![IndexDef {
                original: Some("PRIMARY".into()),
                name: "PRIMARY".into(),
                kind: "PRIMARY".into(),
                columns: vec![IndexColumn { name: "id".into(), length: None, desc: false, expr: None }],
                comment: String::new(),
                ignored: false,
                m: None,
                distance: None,
                overlaps: None,
            }],
            fks: vec![],
            checks: vec![],
            versioned: false,
            periods: vec![],
            partitions: String::new(),
        }
    }

    #[test]
    fn writes_column_definitions() {
        let mut c = col("it's", "varchar(10)");
        c.nullable = true;
        c.default = DefaultValue::Literal("o'k\\".into());
        c.comment = "note".into();
        assert_eq!(column_sql(&c).unwrap(), "`it's` varchar(10) NULL DEFAULT 'o''k\\\\' COMMENT 'note'");
        c.default = DefaultValue::Expr("current_timestamp()".into());
        c.on_update = Some("current_timestamp()".into());
        c.col_type = "timestamp".into();
        assert!(column_sql(&c).unwrap().contains("DEFAULT (current_timestamp()) ON UPDATE current_timestamp()"));
        let g = ColumnDef { generated: Some(Generated { expr: "a + 1".into(), stored: true }), ..col("g", "int") };
        assert_eq!(column_sql(&g).unwrap(), "`g` int AS (a + 1) PERSISTENT");
        let bad = ColumnDef { collation: Some("x; DROP".into()), ..col("c", "text") };
        assert_eq!(column_sql(&bad).unwrap_err().code, "INVALID_NAME");
    }

    #[test]
    fn same_definition_means_no_change() {
        assert!(alter_clauses(&table(), &table(), true).unwrap().is_empty());
        assert!(alter_sql("s", &table(), &table(), Algorithm::Instant, true).unwrap().is_empty());
    }

    #[test]
    fn diffs_columns() {
        let old = table();
        let mut new = table();
        new.columns[1].name = "a2".into(); // rename
        new.columns[2].nullable = true; // modify
        new.columns.insert(1, ColumnDef { original: None, ..col("n", "date") }); // add after id
        new.columns.push(ColumnDef { original: None, ..col("z", "int") });
        assert_eq!(
            alter_clauses(&old, &new, true).unwrap(),
            [
                "ADD COLUMN `n` date NOT NULL AFTER `id`",
                "CHANGE COLUMN `a` `a2` varchar(10) NOT NULL",
                "CHANGE COLUMN `b` `b` int(11) NULL",
                "ADD COLUMN `z` int NOT NULL AFTER `b`",
            ]
        );
        let mut drop = table();
        drop.columns.remove(1);
        assert_eq!(alter_clauses(&old, &drop, true).unwrap(), ["DROP COLUMN `a`"]);
    }

    #[test]
    fn moves_columns() {
        let old = table();
        let mut new = table();
        new.columns.swap(1, 2); // id, b, a
        assert_eq!(
            alter_clauses(&old, &new, true).unwrap(),
            ["CHANGE COLUMN `b` `b` int(11) NOT NULL AFTER `id`", "CHANGE COLUMN `a` `a` varchar(10) NOT NULL AFTER `b`"]
        );
        let mut first = table();
        first.columns.rotate_right(1); // b, id, a
        let c = alter_clauses(&old, &first, true).unwrap();
        assert_eq!(c[0], "CHANGE COLUMN `b` `b` int(11) NOT NULL FIRST");
    }

    #[test]
    fn diffs_indexes_constraints_and_options() {
        let old = TableDef {
            fks: vec![FkDef {
                original: Some("fk".into()),
                name: "fk".into(),
                columns: vec!["b".into()],
                ref_schema: String::new(),
                ref_table: "p".into(),
                ref_columns: vec!["id".into()],
                on_update: "RESTRICT".into(),
                on_delete: "CASCADE".into(),
            }],
            checks: vec![CheckDef { original: Some("c1".into()), name: "c1".into(), clause: "b > 0".into() }],
            ..table()
        };
        let mut new = old.clone();
        new.indexes[0].columns.push(IndexColumn { name: "a".into(), length: Some(4), desc: false, expr: None });
        new.indexes.push(IndexDef { original: None, name: "".into(), kind: "UNIQUE".into(), columns: vec![IndexColumn { name: "b".into(), length: None, desc: true, expr: None }], comment: String::new(), ignored: false, m: None, distance: None, overlaps: None });
        // A functional key part (MySQL 8) is written as its expression in parentheses.
        let part = IndexColumn { name: String::new(), length: None, desc: true, expr: Some("`a` + 1".into()) };
        new.indexes.push(IndexDef { original: None, name: "f".into(), kind: "INDEX".into(), columns: vec![part], comment: String::new(), ignored: false, m: None, distance: None, overlaps: None });
        new.fks[0].on_delete = "set null".into();
        new.checks.clear();
        new.comment = "hi".into();
        assert_eq!(
            alter_clauses(&old, &new, true).unwrap(),
            [
                "DROP FOREIGN KEY `fk`",
                "DROP CONSTRAINT `c1`",
                "DROP PRIMARY KEY",
                "ADD PRIMARY KEY (`id`, `a`(4))",
                "ADD UNIQUE INDEX (`b` DESC)",
                "ADD INDEX `f` ((`a` + 1) DESC)",
                "ADD CONSTRAINT `fk` FOREIGN KEY (`b`) REFERENCES `p` (`id`) ON DELETE SET NULL ON UPDATE RESTRICT",
                "COMMENT='hi'",
            ]
        );
        new.fks[0].on_delete = "EXPLODE".into();
        assert_eq!(alter_clauses(&old, &new, true).unwrap_err().code, "INVALID_NAME");
    }

    #[test]
    fn partitions_sql() {
        let ddl = "CREATE TABLE `pr` (\n  `id` int(11) NOT NULL\n) ENGINE=InnoDB\n PARTITION BY RANGE (`id`)\n(PARTITION `p0` VALUES LESS THAN (10) ENGINE = InnoDB)";
        assert_eq!(partition_clause(ddl), "PARTITION BY RANGE (`id`)\n(PARTITION `p0` VALUES LESS THAN (10) ENGINE = InnoDB)");
        let mysql = "CREATE TABLE `h` (\n  `id` int NOT NULL\n) ENGINE=InnoDB\n/*!50100 PARTITION BY HASH (`id`)\nPARTITIONS 4 */";
        assert_eq!(partition_clause(mysql), "PARTITION BY HASH (`id`)\nPARTITIONS 4");
        assert_eq!(partition_clause("CREATE TABLE `t` (\n  `id` int\n) ENGINE=InnoDB"), "");

        let mut t = table();
        t.partitions = "PARTITION BY HASH (`id`) PARTITIONS 4".into();
        assert!(create_sql("s", &t, true).unwrap().ends_with("COMMENT=''\nPARTITION BY HASH (`id`) PARTITIONS 4"));
        let plain = table();
        assert_eq!(alter_sql("s", &plain, &t, Algorithm::Default, true).unwrap(), ["ALTER TABLE `s`.`t`\n  PARTITION BY HASH (`id`) PARTITIONS 4"]);
        assert_eq!(alter_sql("s", &t, &plain, Algorithm::Default, true).unwrap(), ["ALTER TABLE `s`.`t` REMOVE PARTITIONING"]);
        // A versioned table needs KEEP for it too; WAIT goes on every ALTER of the table.
        let mut v = plain.clone();
        v.versioned = true;
        let mut vp = t.clone();
        vp.versioned = true;
        let w = with_wait(alter_sql("s", &v, &vp, Algorithm::Default, true).unwrap(), "s", "t", Some(3));
        assert_eq!(w, ["SET SESSION system_versioning_alter_history = KEEP", "ALTER TABLE `s`.`t` WAIT 3\n  PARTITION BY HASH (`id`) PARTITIONS 4"]);
    }

    #[test]
    fn lock_wait_on_alters_only() {
        let stmts = vec!["RENAME TABLE `s`.`a` TO `s`.`b`".to_string(), "SET SESSION x = 1".into(), "ALTER TABLE `s`.`b`\n  ADD COLUMN c int".into()];
        let w = with_wait(stmts.clone(), "s", "b", Some(5));
        assert_eq!(w[2], "ALTER TABLE `s`.`b` WAIT 5\n  ADD COLUMN c int");
        assert_eq!(&w[..2], &stmts[..2]);
        assert_eq!(with_wait(stmts.clone(), "s", "b", Some(0))[2], "ALTER TABLE `s`.`b` NOWAIT\n  ADD COLUMN c int");
        assert_eq!(with_wait(stmts.clone(), "s", "b", None), stmts);
    }

    #[test]
    fn versioning_and_periods_sql() {
        let mut t = table();
        t.columns.push(col("d1", "date"));
        t.columns.push(col("d2", "date"));
        t.columns[2].unversioned = true;
        t.periods.push(Period { name: "valid".into(), start: "d1".into(), end: "d2".into() });
        t.indexes[0].overlaps = Some("valid".into());
        t.versioned = true;
        let c = create_sql("s", &t, true).unwrap();
        assert!(c.contains("`b` int(11) NOT NULL WITHOUT SYSTEM VERSIONING"), "{c}");
        assert!(c.contains("PERIOD FOR `valid` (`d1`, `d2`),\n  PRIMARY KEY (`id`, `valid` WITHOUT OVERLAPS)"), "{c}");
        assert!(c.ends_with("COMMENT='' WITH SYSTEM VERSIONING"), "{c}");

        // Explicit row columns: GENERATED ALWAYS AS ROW START / END, SYSTEM_TIME unquoted.
        let mut rs = col("rs", "timestamp(6)");
        rs.generated = Some(Generated { expr: "ROW START".into(), stored: true });
        assert_eq!(column_sql(&rs).unwrap(), "`rs` timestamp(6) GENERATED ALWAYS AS ROW START");
        assert_eq!(period_sql(&Period { name: "SYSTEM_TIME".into(), start: "rs".into(), end: "re".into() }).unwrap(), "PERIOD FOR SYSTEM_TIME (`rs`, `re`)");

        // ALTER of a versioned table: KEEP first; period dropped before its columns, added before its index.
        let old = t.clone();
        let mut new = t.clone();
        new.periods.clear();
        new.indexes[0].overlaps = None;
        new.versioned = false;
        let a = alter_sql("s", &old, &new, Algorithm::Default, true).unwrap();
        assert_eq!(a[0], "SET SESSION system_versioning_alter_history = KEEP");
        assert_eq!(a[1], "ALTER TABLE `s`.`t`\n  DROP PRIMARY KEY,\n  DROP PERIOD FOR `valid`,\n  ADD PRIMARY KEY (`id`),\n  DROP SYSTEM VERSIONING");
        let back = alter_clauses(&new, &old, true).unwrap();
        assert_eq!(back, ["DROP PRIMARY KEY", "ADD PERIOD FOR `valid` (`d1`, `d2`)", "ADD PRIMARY KEY (`id`, `valid` WITHOUT OVERLAPS)", "ADD SYSTEM VERSIONING"]);

        // Server-added parts left out on load.
        let ip = |n: &str| IndexColumn { name: n.into(), length: None, desc: false, expr: None };
        let mut idx = vec![IndexDef { columns: vec![ip("id"), ip("d2"), ip("d1"), ip("re")], ..t.indexes[0].clone() }];
        idx[0].overlaps = None;
        let mut checks = vec![CheckDef { original: None, name: "valid".into(), clause: "`d1` < `d2`".into() }];
        let periods = [Period { name: "SYSTEM_TIME".into(), start: "rs".into(), end: "re".into() }, Period { name: "valid".into(), start: "d1".into(), end: "d2".into() }];
        server_added_out(&periods, &mut idx, &mut checks);
        assert_eq!(idx[0].columns, [ip("id")]);
        assert_eq!(idx[0].overlaps.as_deref(), Some("valid"));
        assert!(checks.is_empty());
    }

    #[test]
    fn keeps_index_flags() {
        let mut old = table();
        old.indexes.push(IndexDef {
            original: Some("ib".into()),
            name: "ib".into(),
            kind: "INDEX".into(),
            columns: vec![IndexColumn { name: "b".into(), length: None, desc: true, expr: None }],
            comment: String::new(),
            ignored: false,
            m: None,
            distance: None,
            overlaps: None,
        });
        old.indexes.push(IndexDef {
            original: Some("ve".into()),
            name: "ve".into(),
            kind: "VECTOR".into(),
            columns: vec![IndexColumn { name: "a".into(), length: None, desc: false, expr: None }],
            comment: String::new(),
            ignored: false,
            m: None,
            distance: None,
            overlaps: None,
        });
        assert!(alter_clauses(&old, &old, true).unwrap().is_empty());

        // Ignoring an index is switched in place, with each server's keyword.
        let mut ign = old.clone();
        ign.indexes[1].ignored = true;
        assert_eq!(alter_clauses(&old, &ign, true).unwrap(), ["ALTER INDEX `ib` IGNORED"]);
        assert_eq!(alter_clauses(&old, &ign, false).unwrap(), ["ALTER INDEX `ib` INVISIBLE"]);
        assert_eq!(alter_clauses(&ign, &old, true).unwrap(), ["ALTER INDEX `ib` NOT IGNORED"]);

        // A rebuilt index keeps DESC and the ignored flag.
        let mut rebuilt = ign.clone();
        rebuilt.indexes[1].comment = "c".into();
        assert_eq!(alter_clauses(&old, &rebuilt, true).unwrap(), ["DROP INDEX `ib`", "ADD INDEX `ib` (`b` DESC) COMMENT 'c' IGNORED"]);

        // VECTOR: rebuilt with its M / DISTANCE options, which are checked.
        let mut dropped = old.clone();
        dropped.indexes.pop();
        assert_eq!(alter_clauses(&old, &dropped, true).unwrap(), ["DROP INDEX `ve`"]);
        let mut edited = old.clone();
        (edited.indexes[2].m, edited.indexes[2].distance) = (Some(8), Some("cosine".into()));
        assert_eq!(alter_clauses(&old, &edited, true).unwrap(), ["DROP INDEX `ve`", "ADD VECTOR INDEX `ve` (`a`) M=8 DISTANCE=cosine"]);
        edited.indexes[2].m = Some(201);
        assert_eq!(alter_clauses(&old, &edited, true).unwrap_err().detail.as_deref(), Some("vector_m"));
        (edited.indexes[2].m, edited.indexes[2].distance) = (None, Some("dot".into()));
        assert_eq!(alter_clauses(&old, &edited, true).unwrap_err().detail.as_deref(), Some("vector_distance"));
    }

    #[test]
    fn builds_full_statements() {
        let mut new = table();
        new.name = "t2".into();
        new.comment = "x".into();
        assert_eq!(
            alter_sql("s", &table(), &new, Algorithm::Online, true).unwrap(),
            ["RENAME TABLE `s`.`t` TO `s`.`t2`", "ALTER TABLE `s`.`t2`\n  COMMENT='x', ALGORITHM=INPLACE, LOCK=NONE"]
        );
        let c = create_sql("s", &table(), true).unwrap();
        assert!(c.starts_with("CREATE TABLE `s`.`t` (\n  `id` int(11) NOT NULL,"), "{c}");
        assert!(c.ends_with("PRIMARY KEY (`id`)\n) ENGINE=InnoDB COLLATE=utf8mb4_general_ci COMMENT=''"), "{c}");
        assert_eq!(drop_sql("s", "v", "view", false).unwrap(), "DROP VIEW `s`.`v`");
        assert_eq!(drop_sql("s", "t", "table", true).unwrap(), "TRUNCATE TABLE `s`.`t`");
        assert!(drop_sql("s", "v", "view", true).is_err());
    }

    #[test]
    fn reads_fk_rules_from_ddl() {
        let ddl = "CREATE TABLE `c` (\n  `p` int(11),\n  CONSTRAINT `1` FOREIGN KEY (`p`) REFERENCES `p` (`id`),\n  CONSTRAINT `fk``x` FOREIGN KEY (`p`) REFERENCES `p` (`id`) ON DELETE SET NULL ON UPDATE CASCADE\n)";
        assert_eq!(fk_rules_from_ddl(ddl, "1"), ("RESTRICT".into(), "RESTRICT".into()));
        assert_eq!(fk_rules_from_ddl(ddl, "fk`x"), ("CASCADE".into(), "SET NULL".into()));
    }

    #[test]
    fn parses_column_defaults() {
        assert_eq!(parse_default(None, true, ""), DefaultValue::None);
        assert_eq!(parse_default(Some("NULL".into()), true, ""), DefaultValue::Null);
        assert_eq!(parse_default(Some("'it''s'".into()), true, ""), DefaultValue::Literal("it's".into()));
        assert_eq!(parse_default(Some("current_timestamp()".into()), true, ""), DefaultValue::Expr("current_timestamp()".into()));
        assert_eq!(parse_default(Some("abc".into()), false, ""), DefaultValue::Literal("abc".into()));
        assert_eq!(parse_default(Some("now()".into()), false, "default_generated"), DefaultValue::Expr("now()".into()));
    }

    fn pool() -> Pool {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        )
    }

    /// Round trip on a real server: what we load must diff to nothing, and our ALTERs must apply.
    /// OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn charsets_and_create_database_on_test_server() {
        let pool = pool();
        let c = charsets(&pool).await.unwrap();
        // The server default collation must be pickable (UCA 14 names on MariaDB 11+).
        let (_, colls) = c.charsets.iter().find(|(cs, _)| *cs == c.default_charset).unwrap();
        assert!(colls.contains(&c.default_collation), "{} not in {colls:?}", c.default_collation);
        run(&pool, &["DROP DATABASE IF EXISTS otaria_newdb".into()]).await.unwrap();
        database_ddl(&pool, "create", "otaria_newdb", &c.default_charset, &c.default_collation).await.unwrap();
        assert_eq!(database_ddl(&pool, "create", "otaria_newdb", "", "").await.unwrap_err().server_code, Some(1007));
        database_ddl(&pool, "alter", "otaria_newdb", "latin1", "latin1_swedish_ci").await.unwrap();
        assert_eq!(database_options(&pool, "otaria_newdb").await.unwrap(), ("latin1".into(), "latin1_swedish_ci".into()));
        database_ddl(&pool, "drop", "otaria_newdb", "", "").await.unwrap();
        assert_eq!(database_options(&pool, "otaria_newdb").await.unwrap_err().code, "NOT_FOUND");
    }

    /// A long ALTER reports its progress, and KILL QUERY stops it.
    #[tokio::test]
    #[ignore]
    async fn alter_progress_on_test_server() {
        let pool = pool();
        run(&pool, &["DROP DATABASE IF EXISTS otaria_prog".into(), "CREATE DATABASE otaria_prog".into(),
            "CREATE TABLE otaria_prog.big (id INT PRIMARY KEY, v CHAR(200)) SELECT seq AS id, REPEAT('x', 200) AS v FROM mysql.seq_1_to_300000".into()])
            .await
            .unwrap();
        let mut conn = pool.get_conn().await.unwrap();
        let id = conn.id();
        let stmts = with_wait(vec!["ALTER TABLE `otaria_prog`.`big`\n  FORCE, ALGORITHM=COPY".into()], "otaria_prog", "big", Some(5));
        let alter = tokio::spawn(async move { run_on(&mut conn, &stmts).await });
        let mut seen = None;
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            if let Some(p) = progress(&pool, id).await.unwrap().filter(|p| p.0 > 0.0) {
                seen = Some(p);
                break;
            }
        }
        let (pct, _, stages) = seen.expect("progress reported");
        assert!(pct > 0.0 && pct <= 100.0 && stages >= 1, "{pct} {stages}");
        crate::query::cancel(&pool, id).await.unwrap();
        let res = alter.await.unwrap();
        assert!(res.is_err() || progress(&pool, id).await.unwrap().is_none(), "{res:?}");
        run(&pool, &["DROP DATABASE otaria_prog".into()]).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn designer_on_test_server() {
        let pool = pool();
        run(
            &pool,
            &[
                "DROP DATABASE IF EXISTS otaria_des".into(),
                "CREATE DATABASE otaria_des".into(),
                "CREATE TABLE otaria_des.p (id INT PRIMARY KEY)".into(),
                "CREATE TABLE otaria_des.t (
                   id INT AUTO_INCREMENT PRIMARY KEY,
                   name VARCHAR(20) COLLATE utf8mb4_bin DEFAULT 'it''s \\\\ ok' COMMENT 'the name',
                   created TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
                   n INT DEFAULT 0 INVISIBLE,
                   doc JSON,
                   twice INT AS (n * 2) VIRTUAL,
                   pid INT,
                   CONSTRAINT positive CHECK (n >= 0),
                   UNIQUE KEY uname (name(5)),
                   CONSTRAINT fk_p FOREIGN KEY (pid) REFERENCES p (id) ON DELETE SET NULL
                 ) COMMENT 'tbl'"
                    .into(),
            ],
        )
        .await
        .unwrap();
        let def = load(&pool, "otaria_des", "t", true).await.unwrap();
        assert_eq!(alter_clauses(&def, &def, true).unwrap(), Vec::<String>::new());
        let name = &def.columns[1];
        assert_eq!(name.default, DefaultValue::Literal("it's \\ ok".into()));
        assert_eq!(name.collation.as_deref(), Some("utf8mb4_bin"));
        assert!(def.columns[3].invisible);
        assert!(def.columns[4].check.as_deref().unwrap().contains("json_valid"));
        assert_eq!(def.columns[2].on_update.as_deref(), Some("current_timestamp()"));
        assert_eq!(def.fks[0].on_delete, "SET NULL");
        assert_eq!(def.checks[0].name, "positive");

        // Rename a column, move it, add one before it, change a default, rename the table.
        let mut new = def.clone();
        new.name = "t2".into();
        let mut created = new.columns.remove(2);
        created.name = "made".into();
        new.columns.insert(1, created);
        new.columns.insert(1, ColumnDef { original: None, ..col("extra", "int(11)") });
        new.columns.iter_mut().find(|c| c.name == "n").unwrap().default = DefaultValue::Expr("5".into());
        new.indexes.push(IndexDef { original: None, name: "ipid".into(), kind: "INDEX".into(), columns: vec![IndexColumn { name: "pid".into(), length: None, desc: false, expr: None }], comment: String::new(), ignored: false, m: None, distance: None, overlaps: None });
        run(&pool, &alter_sql("otaria_des", &def, &new, Algorithm::Default, true).unwrap()).await.unwrap();
        let after = load(&pool, "otaria_des", "t2", true).await.unwrap();
        let names: Vec<&str> = after.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["id", "extra", "made", "name", "n", "doc", "twice", "pid"]);
        // Reloaded definition equals what we asked for (modulo `original` on new items).
        let expected: Vec<String> = new.columns.iter().map(|c| column_sql(c).unwrap()).collect();
        let got: Vec<String> = after.columns.iter().map(|c| column_sql(c).unwrap()).collect();
        assert_eq!(got, expected);

        // INSTANT is refused for a change that needs a rebuild, with our own error code.
        let mut retype = after.clone();
        retype.columns[1].col_type = "varchar(5)".into();
        let e = run(&pool, &alter_sql("otaria_des", &after, &retype, Algorithm::Instant, true).unwrap()).await.unwrap_err();
        assert_eq!(e.code, "ALTER_ALGORITHM", "{e:?}");

        // Index flags and VECTOR options (MariaDB 11.7+): loaded as they are, so nothing changes and
        // editing the table elsewhere leaves them alone.
        run(
            &pool,
            &["CREATE TABLE otaria_des.v (id INT PRIMARY KEY, e VECTOR(3) NOT NULL, a INT, b INT,
                 VECTOR INDEX ve (e) M=8 DISTANCE=cosine, INDEX ia (a) IGNORED, INDEX ib (b DESC))"
                .into()],
        )
        .await
        .unwrap();
        let v = load(&pool, "otaria_des", "v", true).await.unwrap();
        let kinds: Vec<(&str, &str, bool, bool)> =
            v.indexes.iter().map(|i| (i.name.as_str(), i.kind.as_str(), i.ignored, i.columns[0].desc)).collect();
        assert_eq!(kinds, [("PRIMARY", "PRIMARY", false, false), ("ia", "INDEX", true, false), ("ib", "INDEX", false, true), ("ve", "VECTOR", false, false)]);
        assert!(alter_clauses(&v, &v, true).unwrap().is_empty());
        let mut v2 = v.clone();
        v2.columns.push(ColumnDef { original: None, ..col("c", "int(11)") });
        v2.indexes[1].ignored = false; // un-ignore ia in place
        v2.indexes[2].comment = "rebuilt".into(); // rebuild ib: must stay DESC
        run(&pool, &alter_sql("otaria_des", &v, &v2, Algorithm::Default, true).unwrap()).await.unwrap();
        let v3 = load(&pool, "otaria_des", "v", true).await.unwrap();
        assert!(!v3.indexes[1].ignored && v3.indexes[2].columns[0].desc && v3.indexes[2].comment == "rebuilt");
        assert_eq!(v3.indexes[3].kind, "VECTOR");
        assert_eq!((v3.indexes[3].m, v3.indexes[3].distance.as_deref()), (Some(8), Some("cosine")));
        // Rebuilt with new options (and a new index added from the designer).
        let mut v4 = v3.clone();
        v4.indexes[3].distance = Some("euclidean".into());
        run(&pool, &alter_sql("otaria_des", &v3, &v4, Algorithm::Default, true).unwrap()).await.unwrap();
        let ddl = crate::browse::ddl(&pool, "otaria_des", "v", "table").await.unwrap();
        assert!(ddl.contains("`M`='8'") && ddl.contains("`DISTANCE`='euclidean'"), "{ddl}");

        run(&pool, &["DROP DATABASE otaria_des".into()]).await.unwrap();
    }

    /// A server on NO_BACKSLASH_ESCAPES still gets the backslash we meant, and the connection keeps its mode.
    #[tokio::test]
    #[ignore]
    async fn backslash_under_no_backslash_escapes() {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let opts = mysql_async::OptsBuilder::default()
            .ip_or_hostname(p[0])
            .tcp_port(p[1].parse().unwrap())
            .user(Some(p[2]))
            .pass(Some(p[3]))
            .prefer_socket(false)
            .init(vec!["SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'"])
            // No reset on return: the mode stays, as on a server whose global mode has it.
            .pool_opts(mysql_async::PoolOpts::default().with_reset_connection(false).with_constraints(mysql_async::PoolConstraints::new(1, 1).unwrap()));
        let pool = Pool::new(opts);
        let mut c = col("note", "varchar(10)");
        c.original = None;
        c.default = DefaultValue::Literal("x\\y".into());
        let t = TableDef { name: "nbe".into(), comment: "a\\b".into(), columns: vec![col("id", "int(11)"), c], ..table() };
        run(&pool, &["DROP DATABASE IF EXISTS otaria_nbe".into(), "CREATE DATABASE otaria_nbe".into()]).await.unwrap();
        run(&pool, &[create_sql("otaria_nbe", &t, true).unwrap()]).await.unwrap();
        let back = load(&pool, "otaria_nbe", "nbe", true).await.unwrap();
        assert_eq!(back.comment, "a\\b");
        assert_eq!(back.columns[1].default, DefaultValue::Literal("x\\y".into()));
        let mut conn = pool.get_conn().await.unwrap();
        let mode: String = conn.query_first("SELECT @@sql_mode").await.unwrap().unwrap();
        assert_eq!(mode, "NO_BACKSLASH_ESCAPES");
        drop(conn);
        run(&pool, &["DROP DATABASE otaria_nbe".into()]).await.unwrap();
    }

    /// MySQL 8 functional key parts: loaded with their expression, diff to nothing, and a table
    /// created from what was loaded gets the same index back.
    /// OTARIA_TEST_MYSQL="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn functional_index_on_mysql() {
        let spec = std::env::var("OTARIA_TEST_MYSQL").expect("set OTARIA_TEST_MYSQL");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let pool = Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        );
        run(
            &pool,
            &[
                "DROP DATABASE IF EXISTS otaria_fx".into(),
                "CREATE DATABASE otaria_fx".into(),
                "CREATE TABLE otaria_fx.t (id INT PRIMARY KEY, a INT, b VARCHAR(20), INDEX fx (a, (a + 1) DESC, (CONCAT(b, ',', 'it''s', 'b\\\\s'))))".into(),
            ],
        )
        .await
        .unwrap();
        let t = load(&pool, "otaria_fx", "t", false).await.unwrap();
        let fx = t.indexes.iter().find(|i| i.name == "fx").unwrap();
        assert_eq!(fx.columns.len(), 3, "{fx:?}");
        assert_eq!((fx.columns[0].name.as_str(), fx.columns[0].expr.is_none()), ("a", true));
        assert!(fx.columns[1].desc && fx.columns[1].expr.as_deref().is_some_and(|e| e.contains('+')), "{fx:?}");
        assert!(fx.columns[2].expr.as_deref().is_some_and(|e| e.contains("concat")), "{fx:?}");
        assert!(alter_sql("otaria_fx", &t, &t, Algorithm::Default, false).unwrap().is_empty());

        let mut copy = t.clone();
        copy.name = "t2".into();
        run(&pool, &[create_sql("otaria_fx", &copy, false).unwrap()]).await.unwrap();
        let t2 = load(&pool, "otaria_fx", "t2", false).await.unwrap();
        assert_eq!(t2.indexes.iter().find(|i| i.name == "fx").map(|i| &i.columns), Some(&fx.columns));
        run(&pool, &["DROP DATABASE otaria_fx".into()]).await.unwrap();
    }
}
