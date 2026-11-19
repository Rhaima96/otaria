//! Grid edits turned into prepared UPDATE / INSERT / DELETE, applied in one transaction.

use mysql_async::prelude::Queryable;
use mysql_async::{Params, Pool, TxOpts, Value};
use serde::Deserialize;

use crate::browse::ident;
use crate::error::{AppError, AppResult};

/// Column/value pairs; `None` = SQL NULL.
pub type Pairs = Vec<(String, Option<String>)>;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Change {
    /// `key` = primary key values of the row as loaded.
    Update { key: Pairs, set: Pairs },
    Insert { values: Pairs },
    Delete { key: Pairs },
}

#[derive(Debug, PartialEq)]
pub struct Stmt {
    pub sql: String,
    pub params: Vec<Option<String>>,
    /// UPDATE/DELETE must hit exactly one row, otherwise the row changed under us.
    pub one_row: bool,
}

/// Display-only literal for the preview. Never executed: execution binds parameters.
fn literal(v: &Option<String>) -> String {
    match v {
        None => "NULL".into(),
        Some(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''")),
    }
}

/// Build one statement. `inline` = values written as literals (preview) instead of `?`.
pub fn build(schema: &str, table: &str, change: &Change, inline: bool) -> AppResult<Stmt> {
    let target = format!("{}.{}", ident(schema), ident(table));
    let mut params = Vec::new();
    let mut val = |v: &Option<String>| {
        if inline {
            literal(v)
        } else {
            params.push(v.clone());
            "?".to_string()
        }
    };
    let where_ =|key: &Pairs, val: &mut dyn FnMut(&Option<String>) -> String| -> AppResult<String> {
        if key.is_empty() {
            return Err(AppError::new("NO_PRIMARY_KEY", table.to_string()));
        }
        Ok(key.iter().map(|(c, v)| format!("{} = {}", ident(c), val(v))).collect::<Vec<_>>().join(" AND "))
    };
    let (sql, one_row) = match change {
        Change::Update { key, set } => {
            let set = set.iter().map(|(c, v)| format!("{} = {}", ident(c), val(v))).collect::<Vec<_>>().join(", ");
            (format!("UPDATE {target} SET {set} WHERE {}", where_(key, &mut val)?), true)
        }
        Change::Insert { values } => {
            let cols = values.iter().map(|(c, _)| ident(c)).collect::<Vec<_>>().join(", ");
            let vals = values.iter().map(|(_, v)| val(v)).collect::<Vec<_>>().join(", ");
            (format!("INSERT INTO {target} ({cols}) VALUES ({vals})"), false)
        }
        Change::Delete { key } => (format!("DELETE FROM {target} WHERE {}", where_(key, &mut val)?), true),
    };
    drop(val);
    Ok(Stmt { sql, params, one_row })
}

pub fn preview(schema: &str, table: &str, changes: &[Change]) -> AppResult<Vec<String>> {
    changes.iter().map(|c| build(schema, table, c, true).map(|s| s.sql + ";")).collect()
}

/// Run all changes in one transaction; any failure or row mismatch rolls everything back.
pub async fn apply(pool: &Pool, schema: &str, table: &str, changes: &[Change]) -> AppResult<u64> {
    let stmts = changes.iter().map(|c| build(schema, table, c, false)).collect::<AppResult<Vec<_>>>()?;
    let mut conn = pool.get_conn().await?;
    let mut tx = conn.start_transaction(TxOpts::default()).await?;
    let mut total = 0;
    for (i, s) in stmts.into_iter().enumerate() {
        let params = Params::Positional(s.params.into_iter().map(Value::from).collect());
        tx.exec_drop(s.sql, params).await?; // on error, dropping `tx` rolls back
        let n = tx.affected_rows();
        if s.one_row && n != 1 {
            tx.rollback().await?;
            return Err(AppError::new("ROW_CONFLICT", build(schema, table, &changes[i], true)?.sql));
        }
        total += n;
    }
    tx.commit().await?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(c: &str, v: Option<&str>) -> (String, Option<String>) {
        (c.into(), v.map(Into::into))
    }

    #[test]
    fn builds_prepared_statements() {
        let up = Change::Update { key: vec![p("id", Some("7"))], set: vec![p("name", Some("a")), p("note", None)] };
        assert_eq!(
            build("s", "t", &up, false).unwrap(),
            Stmt {
                sql: "UPDATE `s`.`t` SET `name` = ?, `note` = ? WHERE `id` = ?".into(),
                params: vec![Some("a".into()), None, Some("7".into())],
                one_row: true,
            }
        );
        let ins = Change::Insert { values: vec![p("a`b", Some("x"))] };
        let s = build("s", "t", &ins, false).unwrap();
        assert_eq!((s.sql.as_str(), s.one_row), ("INSERT INTO `s`.`t` (`a``b`) VALUES (?)", false));
        let del = Change::Delete { key: vec![p("a", Some("1")), p("b", Some("2"))] };
        assert_eq!(build("s", "t", &del, false).unwrap().sql, "DELETE FROM `s`.`t` WHERE `a` = ? AND `b` = ?");
    }

    #[test]
    fn preview_inlines_escaped_literals() {
        let up = Change::Update { key: vec![p("id", Some("1"))], set: vec![p("n", Some("it's \\ ok")), p("m", None)] };
        assert_eq!(
            preview("s", "t", &[up]).unwrap(),
            vec!["UPDATE `s`.`t` SET `n` = 'it''s \\\\ ok', `m` = NULL WHERE `id` = '1';"]
        );
    }

    /// Live: browse + edit round trip. OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn browse_and_edit_live() {
        use crate::browse;
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let x: Vec<&str> = spec.splitn(4, ':').collect();
        let url = format!("mysql://{}:{}@{}:{}/", x[2], x[3], x[0], x[1]);
        let opts = mysql_async::OptsBuilder::from_opts(mysql_async::Opts::from_url(&url).unwrap()).client_found_rows(true);
        let pool = Pool::new(opts);
        let mut c = pool.get_conn().await.unwrap();
        c.query_drop(
            "DROP DATABASE IF EXISTS otaria_t; CREATE DATABASE otaria_t;
             CREATE TABLE otaria_t.p (id INT PRIMARY KEY AUTO_INCREMENT, name VARCHAR(20), note TEXT) WITH SYSTEM VERSIONING;
             CREATE SEQUENCE otaria_t.s; CREATE VIEW otaria_t.v AS SELECT id FROM otaria_t.p;
             INSERT INTO otaria_t.p (name) VALUES ('a'), ('b');",
        )
        .await
        .unwrap();
        drop(c);

        let objs = browse::objects(&pool, "otaria_t").await.unwrap();
        let kinds: Vec<(&str, &str, bool)> = objs.iter().map(|o| (o.name.as_str(), o.kind.as_str(), o.versioned)).collect();
        assert_eq!(kinds, [("p", "table", true), ("s", "sequence", false), ("v", "view", false)]);
        // Global search: the same objects, with their schema; no information_schema.
        let all = browse::all_objects(&pool).await.unwrap();
        assert_eq!(all.iter().filter(|o| o.schema == "otaria_t").count(), 3);
        assert!(all.iter().all(|o| o.schema != "information_schema"));
        assert_eq!(browse::table_info(&pool, "otaria_t", "p").await.unwrap().primary_key, ["id"]);
        assert!(browse::ddl(&pool, "otaria_t", "s", "sequence").await.unwrap().contains("CREATE SEQUENCE"));

        let changes = vec![
            Change::Update { key: vec![p("id", Some("1"))], set: vec![p("name", Some("a"))] }, // unchanged value: still 1 row
            Change::Update { key: vec![p("id", Some("2"))], set: vec![p("note", Some("it's"))] },
            Change::Insert { values: vec![p("name", Some("c"))] },
        ];
        assert_eq!(apply(&pool, "otaria_t", "p", &changes).await.unwrap(), 3);

        // A row that vanished rolls the whole batch back.
        let bad = vec![
            Change::Delete { key: vec![p("id", Some("1"))] },
            Change::Delete { key: vec![p("id", Some("99"))] },
        ];
        assert_eq!(apply(&pool, "otaria_t", "p", &bad).await.unwrap_err().code, "ROW_CONFLICT");

        let sql = browse::data_sql("otaria_t", "p", "*", None, &[browse::Sort { column: "id".into(), desc: false }], browse::Page::Offset(0), 10, false);
        let rows = browse::data(&pool, &sql, &[], false).await.unwrap();
        assert_eq!(rows.columns, ["id", "name", "note"]);
        assert_eq!(rows.rows.len(), 3);
        assert_eq!(rows.rows[1][2].as_deref(), Some("it's"));
        assert_eq!(rows.rows[0][2], None);
        pool.get_conn().await.unwrap().query_drop("DROP DATABASE otaria_t").await.unwrap();
    }

    #[test]
    fn refuses_rows_without_key() {
        let del = Change::Delete { key: vec![] };
        assert_eq!(build("s", "t", &del, false).unwrap_err().code, "NO_PRIMARY_KEY");
    }
}
