//! Table designer: load a table definition, diff two definitions into one ALTER TABLE, CREATE TABLE,
//! the table-level operations (rename, truncate, drop), and CREATE DATABASE.
//!
//! DDL cannot take parameters. Identifiers go through `ident`, text values (defaults, comments) through
//! `literal`, keywords (engine, collation, FK rules, index kinds) are checked against a whitelist.
//! Types and expressions (generated columns, DEFAULT expressions, CHECK clauses) are the user's own SQL,
//! run with their own privileges, like the grid filter or the SQL editor.

use mysql_async::prelude::Queryable;
use mysql_async::{Pool, Row};
use serde::{Deserialize, Serialize};

use crate::browse::ident;
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
}

/// Quoted text literal. `'` is doubled, `\` doubled for the default sql_mode.
// ponytail: under NO_BACKSLASH_ESCAPES a backslash in a default/comment comes out doubled (wrong value, still safe).
pub fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
}

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
    if !c.comment.is_empty() {
        s += &format!(" COMMENT {}", literal(&c.comment));
    }
    if let Some(chk) = c.check.as_deref().filter(|s| !s.trim().is_empty()) {
        s += &format!(" CHECK ({})", chk.trim());
    }
    Ok(s)
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
            let col = match c.length {
                Some(n) => format!("{}({n})", ident(&c.name)),
                None => ident(&c.name),
            };
            if c.desc { col + " DESC" } else { col }
        })
        .collect();
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
    Ok(o)
}

pub fn create_sql(schema: &str, t: &TableDef, mariadb: bool) -> AppResult<String> {
    nonempty("table_name", &t.name)?;
    if t.columns.is_empty() {
        return Err(AppError { detail: Some("columns".into()), ..AppError::new("REQUIRED", String::new()) });
    }
    let mut parts: Vec<String> = t.columns.iter().map(column_sql).collect::<AppResult<_>>()?;
    for i in &t.indexes {
        parts.push(index_sql(i, mariadb)?);
    }
    for f in &t.fks {
        parts.push(fk_sql(f)?);
    }
    for c in &t.checks {
        parts.push(check_sql(c)?);
    }
    Ok(format!(
        "CREATE TABLE {}.{} (\n  {}\n) {}",
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

    // 3. Adds.
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
    let mut out = Vec::new();
    if new.name.trim() != old.name {
        nonempty("table_name", &new.name)?;
        out.push(rename_sql(schema, &old.name, new.name.trim()));
    }
    let clauses = alter_clauses(old, new, mariadb)?;
    if !clauses.is_empty() {
        let algo = match algo {
            Algorithm::Instant => ", ALGORITHM=INSTANT",
            Algorithm::Online => ", ALGORITHM=INPLACE, LOCK=NONE",
            Algorithm::Default => "",
        };
        out.push(format!("ALTER TABLE {}.{}\n  {}{algo}", ident(schema), ident(new.name.trim()), clauses.join(",\n  ")));
    }
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
pub async fn run(pool: &Pool, stmts: &[String]) -> AppResult<()> {
    let mut conn = pool.get_conn().await?;
    for s in stmts {
        conn.query_drop(s).await?;
    }
    Ok(())
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
    let mut conn = pool.get_conn().await?;
    let opts: Option<(String, String, String)> = conn
        .exec_first(
            "SELECT IFNULL(ENGINE, ''), IFNULL(TABLE_COLLATION, ''), IFNULL(TABLE_COMMENT, '')
               FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
            (schema, table),
        )
        .await?;
    let (engine, collation, comment) = opts.ok_or_else(|| AppError::new("NOT_FOUND", table.to_string()))?;

    // Column-level CHECKs (MariaDB only) are named after their column.
    let mut col_checks: Vec<(String, String)> = Vec::new();
    let mut checks = Vec::new();
    if mariadb {
        let rows: Vec<(String, String, String)> = conn
            .exec(
                "SELECT CONSTRAINT_NAME, CHECK_CLAUSE, LEVEL FROM information_schema.CHECK_CONSTRAINTS
                  WHERE CONSTRAINT_SCHEMA = ? AND TABLE_NAME = ? ORDER BY CONSTRAINT_NAME",
                (schema, table),
            )
            .await?;
        for (name, clause, level) in rows {
            match level.as_str() {
                "Column" => col_checks.push((name, clause)),
                _ => checks.push(CheckDef { original: Some(name.clone()), name, clause }),
            }
        }
    } else {
        // MySQL 8.0.16+; older servers have no such table: no checks then.
        let rows: Vec<(String, String)> = conn
            .exec(
                "SELECT c.CONSTRAINT_NAME, c.CHECK_CLAUSE FROM information_schema.CHECK_CONSTRAINTS c
                   JOIN information_schema.TABLE_CONSTRAINTS t USING (CONSTRAINT_SCHEMA, CONSTRAINT_NAME)
                  WHERE t.TABLE_SCHEMA = ? AND t.TABLE_NAME = ? AND t.CONSTRAINT_TYPE = 'CHECK' ORDER BY 1",
                (schema, table),
            )
            .await
            .unwrap_or_default();
        checks = rows.into_iter().map(|(name, clause)| CheckDef { original: Some(name.clone()), name, clause }).collect();
    }

    let rows: Vec<Row> = conn
        .exec(
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE = 'YES', COLUMN_DEFAULT, LOWER(EXTRA), COLLATION_NAME,
                    COLUMN_COMMENT, IFNULL(GENERATION_EXPRESSION, '')
               FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
            (schema, table),
        )
        .await?;
    let mut columns = Vec::new();
    for mut r in rows {
        let name: String = r.take(0).unwrap_or_default();
        let extra: String = r.take(4).unwrap_or_default();
        let coll: Option<String> = r.take(5).unwrap_or_default();
        let gen_expr: String = r.take(7).unwrap_or_default();
        let generated = (extra.contains("virtual generated") || extra.contains("stored generated"))
            .then(|| Generated { expr: gen_expr, stored: extra.contains("stored") });
        let on_update = extra.find("on update ").map(|i| {
            let rest = &extra[i + 10..];
            rest.split_whitespace().next().unwrap_or_default().to_string()
        });
        columns.push(ColumnDef {
            original: Some(name.clone()),
            check: col_checks.iter().find(|(n, _)| *n == name).map(|(_, c)| c.clone()),
            col_type: r.take(1).unwrap_or_default(),
            nullable: r.take(2).unwrap_or_default(),
            default: if generated.is_some() { DefaultValue::None } else { parse_default(r.take(3).unwrap_or_default(), mariadb, &extra) },
            auto_increment: extra.contains("auto_increment"),
            on_update,
            invisible: extra.contains("invisible"),
            collation: coll.filter(|c| *c != collation),
            comment: r.take(6).unwrap_or_default(),
            generated,
            name,
        });
    }

    // IGNORED exists from MariaDB 10.6 (oldest supported: 10.11); MySQL calls it IS_VISIBLE.
    let ignored = if mariadb { "IGNORED = 'YES'" } else { "IS_VISIBLE = 'NO'" };
    type IndexRow = (String, bool, Option<String>, Option<u32>, String, String, bool, bool);
    let rows: Vec<IndexRow> = conn
        .exec(
            format!(
                "SELECT INDEX_NAME, NON_UNIQUE = 1, COLUMN_NAME, SUB_PART, INDEX_TYPE, IFNULL(INDEX_COMMENT, ''),
                        IFNULL(COLLATION = 'D', 0), {ignored}
                   FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
                  ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX"
            ),
            (schema, table),
        )
        .await?;
    let mut indexes: Vec<IndexDef> = Vec::new();
    for (name, non_unique, col, sub_part, itype, icomment, desc, ignored) in rows {
        // ponytail: functional index parts (MySQL, COLUMN_NAME NULL) are skipped; such an index is shown without them.
        let Some(col) = col else { continue };
        let column = IndexColumn { name: col, length: sub_part, desc };
        match indexes.last_mut().filter(|i| i.name == name) {
            Some(i) => i.columns.push(column),
            None => indexes.push(IndexDef {
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
            }),
        }
    }
    // M and DISTANCE are only in SHOW CREATE TABLE: without them a rebuilt vector index could change.
    if indexes.iter().any(|i| i.kind == "VECTOR") {
        let found = crate::vector::vector_indexes(&crate::browse::ddl_on(&mut conn, schema, table, "table").await?);
        for i in indexes.iter_mut().filter(|i| i.kind == "VECTOR") {
            if let Some((_, v)) = found.iter().find(|(_, v)| v.name == i.name) {
                (i.m, i.distance) = (v.m, v.distance.clone());
            }
        }
    }

    // LEFT JOIN: with SELECT only, MariaDB lists the FK in KEY_COLUMN_USAGE but hides its
    // REFERENTIAL_CONSTRAINTS row; the rules then come from SHOW CREATE TABLE (visible with SELECT).
    type FkRow = (String, String, String, String, String, Option<String>, Option<String>);
    let rows: Vec<FkRow> = conn
        .exec(
            "SELECT k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME,
                    k.REFERENCED_COLUMN_NAME, r.UPDATE_RULE, r.DELETE_RULE
               FROM information_schema.KEY_COLUMN_USAGE k
               LEFT JOIN information_schema.REFERENTIAL_CONSTRAINTS r
                 ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME
              WHERE k.TABLE_SCHEMA = ? AND k.TABLE_NAME = ? AND k.REFERENCED_TABLE_NAME IS NOT NULL
              ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
            (schema, table),
        )
        .await?;
    let ddl = match rows.iter().any(|r| r.5.is_none()) {
        true => crate::browse::ddl_on(&mut conn, schema, table, "table").await?,
        false => String::new(),
    };
    let mut fks: Vec<FkDef> = Vec::new();
    for (name, col, rs, rt, rc, upd, del) in rows {
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

    Ok(TableDef { name: table.to_string(), engine, collation, comment, columns, indexes, fks, checks })
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

/// `CREATE DATABASE name [CHARACTER SET cs] [COLLATE coll]`; empty charset or collation = server default.
pub fn create_database_sql(name: &str, charset: &str, collation: &str) -> AppResult<String> {
    if name.trim().is_empty() {
        return Err(AppError { detail: Some("name".into()), ..AppError::new("INVALID_NAME", String::new()) });
    }
    let mut s = format!("CREATE DATABASE {}", ident(name.trim()));
    if !charset.is_empty() {
        s += &format!(" CHARACTER SET {}", word("charset", charset)?);
    }
    if !collation.is_empty() {
        s += &format!(" COLLATE {}", word("collation", collation)?);
    }
    Ok(s)
}

pub async fn create_database(pool: &Pool, name: &str, charset: &str, collation: &str) -> AppResult<()> {
    let sql = create_database_sql(name, charset, collation)?;
    Ok(pool.get_conn().await?.query_drop(sql).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_database_statement() {
        assert_eq!(create_database_sql(" shop ", "", "").unwrap(), "CREATE DATABASE `shop`");
        assert_eq!(
            create_database_sql("a`b", "utf8mb4", "utf8mb4_uca1400_ai_ci").unwrap(),
            "CREATE DATABASE `a``b` CHARACTER SET utf8mb4 COLLATE utf8mb4_uca1400_ai_ci"
        );
        assert_eq!(create_database_sql("x", "utf8; DROP", "").unwrap_err().code, "INVALID_NAME");
        assert_eq!(create_database_sql("  ", "", "").unwrap_err().code, "INVALID_NAME");
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
                columns: vec![IndexColumn { name: "id".into(), length: None, desc: false }],
                comment: String::new(),
                ignored: false,
                m: None,
                distance: None,
            }],
            fks: vec![],
            checks: vec![],
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
        new.indexes[0].columns.push(IndexColumn { name: "a".into(), length: Some(4), desc: false });
        new.indexes.push(IndexDef { original: None, name: "".into(), kind: "UNIQUE".into(), columns: vec![IndexColumn { name: "b".into(), length: None, desc: true }], comment: String::new(), ignored: false, m: None, distance: None });
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
                "ADD CONSTRAINT `fk` FOREIGN KEY (`b`) REFERENCES `p` (`id`) ON DELETE SET NULL ON UPDATE RESTRICT",
                "COMMENT='hi'",
            ]
        );
        new.fks[0].on_delete = "EXPLODE".into();
        assert_eq!(alter_clauses(&old, &new, true).unwrap_err().code, "INVALID_NAME");
    }

    #[test]
    fn keeps_index_flags() {
        let mut old = table();
        old.indexes.push(IndexDef {
            original: Some("ib".into()),
            name: "ib".into(),
            kind: "INDEX".into(),
            columns: vec![IndexColumn { name: "b".into(), length: None, desc: true }],
            comment: String::new(),
            ignored: false,
            m: None,
            distance: None,
        });
        old.indexes.push(IndexDef {
            original: Some("ve".into()),
            name: "ve".into(),
            kind: "VECTOR".into(),
            columns: vec![IndexColumn { name: "a".into(), length: None, desc: false }],
            comment: String::new(),
            ignored: false,
            m: None,
            distance: None,
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
        create_database(&pool, "otaria_newdb", &c.default_charset, &c.default_collation).await.unwrap();
        assert_eq!(create_database(&pool, "otaria_newdb", "", "").await.unwrap_err().server_code, Some(1007));
        run(&pool, &["DROP DATABASE otaria_newdb".into()]).await.unwrap();
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
        new.indexes.push(IndexDef { original: None, name: "ipid".into(), kind: "INDEX".into(), columns: vec![IndexColumn { name: "pid".into(), length: None, desc: false }], comment: String::new(), ignored: false, m: None, distance: None });
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
}
