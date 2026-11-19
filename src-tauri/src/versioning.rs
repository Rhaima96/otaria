//! MariaDB temporal tables beyond phase 3b: system versioning on/off, history purge, SYSTEM_TIME
//! partitions, comparison of two moments, application-time periods. MariaDB only (hidden on MySQL).
//!
//! Like the sequence editor, the UI sends an `Action`, never SQL: the statements shown in the preview
//! and the ones executed are rebuilt from the same inputs. DDL commits by itself: no transaction.

use std::collections::HashMap;

use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use serde::{Deserialize, Serialize};

use crate::browse::{self, ident, Rows};
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Period {
    pub name: String,
    pub start: String,
    pub end: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Partition {
    pub name: String,
    /// INTERVAL partitions: upper bound (UTC); `CURRENT` for the current-data partition; None for LIMIT.
    pub description: Option<String>,
    /// Estimate from information_schema.
    pub rows: u64,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub versioned: bool,
    /// ROW START / ROW END columns; `implicit` = invisible ROW_START / ROW_END.
    pub system: Option<Period>,
    pub implicit: bool,
    /// Application-time periods (`PERIOD FOR name (start, end)`).
    pub periods: Vec<Period>,
    /// SYSTEM_TIME partitions, oldest first, `pn` (CURRENT) last.
    pub partitions: Vec<Partition>,
    /// Partitioned some other way (RANGE, HASH…): partition actions are not offered.
    pub other_partitioning: bool,
    pub history_rows: u64,
    /// Oldest ROW START in the table (UTC).
    pub oldest: Option<String>,
}

/// Every `PERIOD FOR name (start, end)` of a `SHOW CREATE TABLE`, SYSTEM_TIME included.
/// (`information_schema.PERIODS` only exists from 11.4.)
pub fn periods(ddl: &str) -> Vec<Period> {
    ddl.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("PERIOD FOR ")?;
            let names = browse::quoted_names(rest);
            let (name, cols) = if rest.starts_with("SYSTEM_TIME") { ("SYSTEM_TIME", &names[..]) } else { (names.first()?.as_str(), names.get(1..)?) };
            Some(Period { name: name.into(), start: cols.first()?.to_string(), end: cols.get(1)?.to_string() })
        })
        .collect()
}

pub async fn state(pool: &Pool, schema: &str, table: &str) -> AppResult<State> {
    let mut conn = pool.get_conn().await?;
    let ddl = browse::ddl_on(&mut conn, schema, table, "table").await?;
    let mut st = State { versioned: ddl.contains(" WITH SYSTEM VERSIONING"), ..State::default() };
    for p in periods(&ddl) {
        if p.name == "SYSTEM_TIME" {
            st.system = Some(p);
        } else {
            st.periods.push(p);
        }
    }
    if st.versioned && st.system.is_none() {
        st.implicit = true;
        st.system = Some(Period { name: "SYSTEM_TIME".into(), start: "ROW_START".into(), end: "ROW_END".into() });
    }
    type PartRow = (Option<String>, Option<String>, Option<String>, Option<u64>);
    let parts: Vec<PartRow> = conn
        .exec(
            "SELECT PARTITION_NAME, PARTITION_METHOD, PARTITION_DESCRIPTION, TABLE_ROWS FROM information_schema.PARTITIONS
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND PARTITION_NAME IS NOT NULL ORDER BY PARTITION_ORDINAL_POSITION",
            (schema, table),
        )
        .await?;
    for (name, method, description, rows) in parts {
        match method.as_deref() {
            Some("SYSTEM_TIME") => st.partitions.push(Partition { name: name.unwrap_or_default(), description, rows: rows.unwrap_or(0) }),
            _ => st.other_partitioning = true,
        }
    }
    if let Some(sys) = &st.system {
        // ponytail: two COUNT(*) scans, run only when the panel opens; fine until tables get huge.
        let t = format!("{}.{}", ident(schema), ident(table));
        conn.query_drop("SET time_zone = '+00:00'").await?;
        let all: Option<(u64, Option<String>)> =
            conn.query_first(format!("SELECT COUNT(*), CAST(MIN({}) AS CHAR) FROM {t} FOR SYSTEM_TIME ALL", ident(&sys.start))).await?;
        let current: Option<u64> = conn.query_first(format!("SELECT COUNT(*) FROM {t}")).await?;
        let (n, oldest) = all.unwrap_or((0, None));
        st.history_rows = n.saturating_sub(current.unwrap_or(0));
        st.oldest = oldest;
    }
    Ok(st)
}

/// `YYYY-MM-DD HH:MM:SS[.ffffff]`, checked digit by digit: the only user text that reaches DDL.
pub fn timestamp(s: &str) -> AppResult<String> {
    let s = s.trim();
    let ok = s.len() >= 19
        && s.len() <= 26
        && s.char_indices().all(|(i, c)| match i {
            4 | 7 => c == '-',
            10 => c == ' ',
            13 | 16 => c == ':',
            19 => c == '.' && s.len() > 20,
            _ => c.is_ascii_digit(),
        });
    if ok { Ok(s.to_string()) } else { Err(AppError::new("INVALID_TIMESTAMP", s.to_string())) }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartSpec {
    /// INTERVAL n unit, else LIMIT n rows per history partition.
    pub interval: bool,
    pub n: u64,
    /// HOUR | DAY | WEEK | MONTH | YEAR (INTERVAL only).
    pub unit: String,
    /// New history partitions created as needed (10.9+).
    pub auto: bool,
    /// Total partitions, current one included (ignored with AUTO); None = server default (2).
    pub partitions: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Action {
    Enable,
    Disable,
    /// UTC timestamp.
    Purge { before: String },
    Partition(PartSpec),
    Unpartition,
    DropPartition { name: String },
    AddPeriod { name: String, start: String, end: String },
    DropPeriod { name: String },
}

/// Statements for `action`, run in order on one connection (with time_zone = '+00:00').
pub fn statements(schema: &str, table: &str, action: &Action, st: &State) -> AppResult<Vec<String>> {
    let t = format!("ALTER TABLE {}.{}", ident(schema), ident(table));
    let need = |ok: bool| if ok { Ok(()) } else { Err(AppError::new("NOT_VERSIONED", table.to_string())) };
    Ok(match action {
        Action::Enable => vec![format!("{t} ADD SYSTEM VERSIONING")],
        Action::Disable => {
            need(st.versioned)?;
            let mut out = Vec::new();
            if !st.partitions.is_empty() {
                out.push(format!("{t} REMOVE PARTITIONING"));
            }
            match (&st.system, st.implicit) {
                // Explicit ROW START / ROW END columns must go in the same ALTER, which the server only
                // allows with system_versioning_alter_history = KEEP.
                (Some(sys), false) => {
                    out.push("SET SESSION system_versioning_alter_history = KEEP".into());
                    out.push(format!("{t} DROP COLUMN {}, DROP COLUMN {}, DROP SYSTEM VERSIONING", ident(&sys.start), ident(&sys.end)));
                }
                _ => out.push(format!("{t} DROP SYSTEM VERSIONING")),
            }
            out
        }
        Action::Purge { before } => {
            need(st.versioned)?;
            vec![format!("DELETE HISTORY FROM {}.{} BEFORE SYSTEM_TIME TIMESTAMP '{}'", ident(schema), ident(table), timestamp(before)?)]
        }
        Action::Partition(p) => {
            need(st.versioned)?;
            if p.n == 0 {
                return Err(AppError::new("INVALID_NUMBER", "0"));
            }
            let by = if p.interval {
                let unit = ["HOUR", "DAY", "WEEK", "MONTH", "YEAR"].into_iter().find(|u| u.eq_ignore_ascii_case(&p.unit)).ok_or_else(|| AppError::new("INVALID_NUMBER", p.unit.clone()))?;
                format!("INTERVAL {} {unit}", p.n)
            } else {
                format!("LIMIT {}", p.n)
            };
            let count = match (p.auto, p.partitions) {
                (true, _) => " AUTO".to_string(),
                (false, Some(k)) if k >= 2 => format!(" PARTITIONS {k}"),
                (false, Some(k)) => return Err(AppError::new("INVALID_NUMBER", k.to_string())),
                (false, None) => String::new(),
            };
            vec![format!("{t} PARTITION BY SYSTEM_TIME {by}{count}")]
        }
        Action::Unpartition => vec![format!("{t} REMOVE PARTITIONING")],
        Action::DropPartition { name } => {
            // Only an existing history partition: the current one cannot be dropped.
            let history: Vec<&Partition> = st.partitions.iter().filter(|p| p.description.as_deref() != Some("CURRENT")).collect();
            if !history.iter().any(|p| &p.name == name) {
                return Err(AppError::new("NOT_FOUND", name.clone()));
            }
            // The server keeps at least one history partition (error 4128).
            if history.len() < 2 {
                return Err(AppError::new("LAST_HISTORY_PARTITION", name.clone()));
            }
            vec![format!("{t} DROP PARTITION {}", ident(name))]
        }
        Action::AddPeriod { name, start, end } => {
            if name.trim().is_empty() || name.eq_ignore_ascii_case("SYSTEM_TIME") {
                return Err(AppError::new("INVALID_NAME", name.clone()));
            }
            vec![format!("{t} ADD PERIOD FOR {} ({}, {})", ident(name.trim()), ident(start), ident(end))]
        }
        Action::DropPeriod { name } => {
            if !st.periods.iter().any(|p| &p.name == name) {
                return Err(AppError::new("NOT_FOUND", name.clone()));
            }
            vec![format!("{t} DROP PERIOD FOR {}", ident(name))]
        }
    })
}

pub async fn preview(pool: &Pool, schema: &str, table: &str, action: &Action) -> AppResult<Vec<String>> {
    statements(schema, table, action, &state(pool, schema, table).await?)
}

/// Re-reads the table state, rebuilds the statements and runs them in order; stops at the first error.
pub async fn apply(pool: &Pool, schema: &str, table: &str, action: &Action) -> AppResult<()> {
    let stmts = preview(pool, schema, table, action).await?;
    let mut conn = pool.get_conn().await?;
    conn.query_drop("SET time_zone = '+00:00'").await?;
    for s in &stmts {
        conn.query_drop(s).await.map_err(|e| AppError { detail: Some(s.clone()), ..AppError::from(e) })?;
    }
    Ok(())
}

/// Template for application-time edits, opened in the SQL editor.
pub fn portion_sql(schema: &str, table: &str, period: &str) -> String {
    let t = format!("{}.{}", ident(schema), ident(table));
    let p = ident(period);
    // The UI explains the split and the WITHOUT OVERLAPS key (translated): no comments in the SQL.
    format!(
        "UPDATE {t} FOR PORTION OF {p} FROM '2026-01-01' TO '2026-02-01'\n  SET `column` = 'value'\n  WHERE `id` = 1;\n\n\
         DELETE FROM {t} FOR PORTION OF {p} FROM '2026-01-01' TO '2026-02-01'\n  WHERE `id` = 1;\n"
    )
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiffRow {
    /// added | removed | changed
    pub kind: &'static str,
    pub before: Option<Vec<Option<String>>>,
    pub after: Option<Vec<Option<String>>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diff {
    pub columns: Vec<String>,
    pub rows: Vec<DiffRow>,
    /// One side had more than COMPARE_CAP rows: the diff covers the first ones only.
    pub truncated: bool,
}

/// Rows read per side.
pub const COMPARE_CAP: usize = 10_000;

/// Rows of `a` and `b` matched on the key columns: added in b, removed from a, or changed. Order: b, then removed.
pub fn diff(a: &Rows, b: &Rows, key: &[usize]) -> Vec<DiffRow> {
    let k = |r: &Vec<Option<String>>| key.iter().map(|&i| r[i].clone()).collect::<Vec<_>>();
    let mut before: HashMap<Vec<Option<String>>, &Vec<Option<String>>> = a.rows.iter().map(|r| (k(r), r)).collect();
    let mut out = Vec::new();
    for r in &b.rows {
        match before.remove(&k(r)) {
            None => out.push(DiffRow { kind: "added", before: None, after: Some(r.clone()) }),
            Some(old) if old != r => out.push(DiffRow { kind: "changed", before: Some(old.clone()), after: Some(r.clone()) }),
            Some(_) => {}
        }
    }
    for r in &a.rows {
        if before.contains_key(&k(r)) {
            out.push(DiffRow { kind: "removed", before: Some(r.clone()), after: None });
        }
    }
    out
}

/// The table as of `a` and as of `b` (UTC), compared on its primary key.
pub async fn compare(pool: &Pool, schema: &str, table: &str, key: &[String], a: &str, b: &str) -> AppResult<Diff> {
    if key.is_empty() {
        return Err(AppError::new("NO_PRIMARY_KEY", table.to_string()));
    }
    let order: Vec<String> = key.iter().map(|c| ident(c)).collect();
    let read = |var: &'static str, at: String| {
        let sql = format!(
            "SELECT * FROM {}.{} FOR SYSTEM_TIME AS OF TIMESTAMP {var} ORDER BY {} LIMIT {}",
            ident(schema),
            ident(table),
            order.join(", "),
            COMPARE_CAP + 1
        );
        async move { browse::data(pool, &sql, &[(var, Some(at))]).await }
    };
    let (mut ra, mut rb) = (read("@otaria_a", timestamp(a)?).await?, read("@otaria_b", timestamp(b)?).await?);
    let truncated = ra.rows.len() > COMPARE_CAP || rb.rows.len() > COMPARE_CAP;
    ra.rows.truncate(COMPARE_CAP);
    rb.rows.truncate(COMPARE_CAP);
    let idx: Vec<usize> = key.iter().filter_map(|c| rb.columns.iter().position(|x| x == c)).collect();
    if idx.len() != key.len() {
        return Err(AppError::new("NO_PRIMARY_KEY", table.to_string()));
    }
    Ok(Diff { rows: diff(&ra, &rb, &idx), columns: rb.columns, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DDL: &str = "CREATE TABLE `t` (
  `id` int(11) NOT NULL,
  `d1` date NOT NULL,
  `d2` date NOT NULL,
  `rs` timestamp(6) GENERATED ALWAYS AS ROW START,
  `re` timestamp(6) GENERATED ALWAYS AS ROW END,
  PERIOD FOR `app` (`d1`, `d2`),
  PERIOD FOR SYSTEM_TIME (`rs`, `re`),
  PRIMARY KEY (`id`,`app` WITHOUT OVERLAPS)
) ENGINE=InnoDB WITH SYSTEM VERSIONING";

    fn p(name: &str, start: &str, end: &str) -> Period {
        Period { name: name.into(), start: start.into(), end: end.into() }
    }

    #[test]
    fn reads_periods_from_ddl() {
        assert_eq!(periods(DDL), [p("app", "d1", "d2"), p("SYSTEM_TIME", "rs", "re")]);
        assert!(periods("CREATE TABLE `x` (`id` int) WITH SYSTEM VERSIONING").is_empty());
    }

    #[test]
    fn validates_timestamps() {
        assert!(timestamp("2026-11-18 10:00:00").is_ok());
        assert!(timestamp("2026-11-18 10:00:00.123456").is_ok());
        for bad in ["2026-11-18", "2026-11-18 10:00:00.", "2026-11-18 10:00:00' OR 1 --", "2026-11-18T10:00:00", "x"] {
            assert_eq!(timestamp(bad).unwrap_err().code, "INVALID_TIMESTAMP", "{bad}");
        }
    }

    #[test]
    fn builds_statements() {
        let explicit = State { versioned: true, system: Some(p("SYSTEM_TIME", "rs", "re")), periods: vec![p("app", "d1", "d2")], ..State::default() };
        let mut implicit = State { versioned: true, implicit: true, system: Some(p("SYSTEM_TIME", "ROW_START", "ROW_END")), ..State::default() };
        implicit.partitions = vec![
            Partition { name: "p0".into(), description: None, rows: 1 },
            Partition { name: "pn".into(), description: Some("CURRENT".into()), rows: 2 },
        ];
        let off = State::default();
        let s = |a: Action, st: &State| statements("s", "t", &a, st);

        assert_eq!(s(Action::Enable, &off).unwrap(), ["ALTER TABLE `s`.`t` ADD SYSTEM VERSIONING"]);
        assert_eq!(s(Action::Disable, &implicit).unwrap(), ["ALTER TABLE `s`.`t` REMOVE PARTITIONING", "ALTER TABLE `s`.`t` DROP SYSTEM VERSIONING"]);
        assert_eq!(
            s(Action::Disable, &explicit).unwrap(),
            ["SET SESSION system_versioning_alter_history = KEEP", "ALTER TABLE `s`.`t` DROP COLUMN `rs`, DROP COLUMN `re`, DROP SYSTEM VERSIONING"]
        );
        assert_eq!(s(Action::Disable, &off).unwrap_err().code, "NOT_VERSIONED");
        assert_eq!(
            s(Action::Purge { before: "2026-01-01 00:00:00".into() }, &implicit).unwrap(),
            ["DELETE HISTORY FROM `s`.`t` BEFORE SYSTEM_TIME TIMESTAMP '2026-01-01 00:00:00'"]
        );
        let part = |interval, n, unit: &str, auto, partitions| Action::Partition(PartSpec { interval, n, unit: unit.into(), auto, partitions });
        assert_eq!(s(part(true, 1, "month", true, None), &implicit).unwrap(), ["ALTER TABLE `s`.`t` PARTITION BY SYSTEM_TIME INTERVAL 1 MONTH AUTO"]);
        assert_eq!(s(part(false, 1000, "", false, Some(3)), &implicit).unwrap(), ["ALTER TABLE `s`.`t` PARTITION BY SYSTEM_TIME LIMIT 1000 PARTITIONS 3"]);
        assert!(s(part(true, 1, "DECADE", false, None), &implicit).is_err());
        assert!(s(part(true, 0, "DAY", false, None), &implicit).is_err());
        assert!(s(part(false, 5, "", false, Some(1)), &implicit).is_err());
        assert_eq!(s(Action::DropPartition { name: "p0".into() }, &implicit).unwrap_err().code, "LAST_HISTORY_PARTITION");
        implicit.partitions.insert(1, Partition { name: "p1".into(), description: None, rows: 0 });
        assert_eq!(s(Action::DropPartition { name: "p0".into() }, &implicit).unwrap(), ["ALTER TABLE `s`.`t` DROP PARTITION `p0`"]);
        assert_eq!(s(Action::DropPartition { name: "pn".into() }, &implicit).unwrap_err().code, "NOT_FOUND");
        assert_eq!(
            s(Action::AddPeriod { name: "va`l".into(), start: "d1".into(), end: "d2".into() }, &off).unwrap(),
            ["ALTER TABLE `s`.`t` ADD PERIOD FOR `va``l` (`d1`, `d2`)"]
        );
        assert!(s(Action::AddPeriod { name: "system_time".into(), start: "a".into(), end: "b".into() }, &off).is_err());
        assert_eq!(s(Action::DropPeriod { name: "app".into() }, &explicit).unwrap(), ["ALTER TABLE `s`.`t` DROP PERIOD FOR `app`"]);
        assert_eq!(s(Action::DropPeriod { name: "nope".into() }, &explicit).unwrap_err().code, "NOT_FOUND");
    }

    #[test]
    fn diffs_rows_on_key() {
        let rows = |r: &[[&str; 2]]| Rows { columns: vec!["id".into(), "v".into()], rows: r.iter().map(|x| x.iter().map(|s| Some(s.to_string())).collect()).collect() };
        let a = rows(&[["1", "a"], ["2", "b"], ["3", "c"]]);
        let b = rows(&[["1", "a"], ["2", "B"], ["4", "d"]]);
        let kinds: Vec<(&str, Option<String>)> = diff(&a, &b, &[0]).iter().map(|d| (d.kind, d.after.as_ref().or(d.before.as_ref()).unwrap()[0].clone())).collect();
        assert_eq!(kinds, [("changed", Some("2".into())), ("added", Some("4".into())), ("removed", Some("3".into()))]);
    }

    #[test]
    fn portion_template_quotes_names() {
        let sql = portion_sql("s", "t`x", "app");
        assert!(sql.contains("UPDATE `s`.`t``x` FOR PORTION OF `app` FROM") && sql.contains("DELETE FROM `s`.`t``x` FOR PORTION OF `app`"));
    }

    /// OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn versioning_on_test_server() {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let pool = Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        );
        let mut conn = pool.get_conn().await.unwrap();
        conn.query_drop(
            "DROP DATABASE IF EXISTS otaria_vt; CREATE DATABASE otaria_vt; USE otaria_vt;
             CREATE TABLE a (id INT PRIMARY KEY, v INT, d1 DATE, d2 DATE);
             INSERT INTO a VALUES (1, 1, '2026-01-01', '2026-12-31'), (2, 2, '2026-01-01', '2026-06-30');
             CREATE TABLE e (id INT PRIMARY KEY, rs TIMESTAMP(6) GENERATED ALWAYS AS ROW START,
               re TIMESTAMP(6) GENERATED ALWAYS AS ROW END, PERIOD FOR SYSTEM_TIME (rs, re)) WITH SYSTEM VERSIONING;",
        )
        .await
        .unwrap();
        let run = |a: Action| {
            let pool = pool.clone();
            async move { apply(&pool, "otaria_vt", "a", &a).await }
        };
        assert!(!state(&pool, "otaria_vt", "a").await.unwrap().versioned);
        run(Action::Enable).await.unwrap();
        // A moment between the two versions (UTC, like the UI sends).
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let t1: String = conn.query_first("SELECT CAST(UTC_TIMESTAMP(6) AS CHAR)").await.unwrap().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        conn.query_drop("UPDATE a SET v = 10 WHERE id = 1; DELETE FROM a WHERE id = 2; INSERT INTO a VALUES (3, 3, '2026-01-01', '2026-02-01')").await.unwrap();
        let t2: String = conn.query_first("SELECT CAST(UTC_TIMESTAMP(6) AS CHAR)").await.unwrap().unwrap();

        let st = state(&pool, "otaria_vt", "a").await.unwrap();
        assert!(st.versioned && st.implicit && st.oldest.is_some());
        assert_eq!(st.history_rows, 2);
        let d = compare(&pool, "otaria_vt", "a", &["id".into()], &t1, &t2).await.unwrap();
        let kinds: Vec<&str> = d.rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, ["changed", "added", "removed"]);

        run(Action::Partition(PartSpec { interval: false, n: 1000, unit: String::new(), auto: false, partitions: Some(3) })).await.unwrap();
        let st = state(&pool, "otaria_vt", "a").await.unwrap();
        assert_eq!(st.partitions.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["p0", "p1", "pn"]);
        run(Action::DropPartition { name: "p0".into() }).await.unwrap();
        run(Action::Purge { before: "2100-01-01 00:00:00".into() }).await.unwrap();
        assert_eq!(state(&pool, "otaria_vt", "a").await.unwrap().history_rows, 0);

        run(Action::AddPeriod { name: "app".into(), start: "d1".into(), end: "d2".into() }).await.unwrap();
        assert_eq!(state(&pool, "otaria_vt", "a").await.unwrap().periods, [Period { name: "app".into(), start: "d1".into(), end: "d2".into() }]);
        run(Action::DropPeriod { name: "app".into() }).await.unwrap();
        run(Action::Disable).await.unwrap();
        let st = state(&pool, "otaria_vt", "a").await.unwrap();
        assert!(!st.versioned && st.partitions.is_empty());

        // Explicit period columns go away with versioning.
        apply(&pool, "otaria_vt", "e", &Action::Disable).await.unwrap();
        assert!(!browse::ddl(&pool, "otaria_vt", "e", "table").await.unwrap().contains("`rs`"));
        let err = apply(&pool, "otaria_vt", "e", &Action::Purge { before: "2100-01-01 00:00:00".into() }).await.unwrap_err();
        assert_eq!(err.code, "NOT_VERSIONED");
        conn.query_drop("DROP DATABASE otaria_vt").await.unwrap();
    }
}
