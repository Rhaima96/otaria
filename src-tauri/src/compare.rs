//! Schema comparison: the tables of a source schema against a target schema (same or another
//! connection), and the script that turns the target into the source. Reuses the designer's
//! loader and diff: both sides load with `original = name`, so columns, indexes and constraints
//! match by name (a renamed column shows as drop + add).

use mysql_async::Pool;
use serde::Serialize;

use crate::browse;
use crate::designer::{self, Algorithm, TableDef};
use crate::error::AppResult;

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TableDiff {
    pub name: String,
    /// create (only in source) | alter (differs) | drop (only in target)
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    pub diffs: Vec<TableDiff>,
    /// Empty when nothing differs.
    pub sql: String,
}

/// Diff of two loaded schemas. DROP TABLE comes out commented: deleting data is left to the user.
pub fn compare(target_schema: &str, src: &[TableDef], tgt: &[TableDef], mariadb: bool) -> AppResult<Comparison> {
    let mut diffs = Vec::new();
    let mut stmts = Vec::new();
    for s in src {
        match tgt.iter().find(|t| t.name == s.name) {
            None => {
                diffs.push(TableDiff { name: s.name.clone(), status: "create" });
                stmts.push(format!("{};", designer::create_sql(target_schema, s, mariadb)?));
            }
            Some(t) => {
                // A source column the target lacks is new to the diff (original = None), not a rename.
                let mut s = s.clone();
                for c in &mut s.columns {
                    c.original = t.columns.iter().any(|o| o.name == c.name).then(|| c.name.clone());
                }
                let alter = designer::alter_sql(target_schema, t, &s, Algorithm::Default, mariadb)?;
                if !alter.is_empty() {
                    diffs.push(TableDiff { name: s.name.clone(), status: "alter" });
                    stmts.extend(alter.into_iter().map(|a| format!("{a};")));
                }
            }
        }
    }
    for t in tgt.iter().filter(|t| !src.iter().any(|s| s.name == t.name)) {
        diffs.push(TableDiff { name: t.name.clone(), status: "drop" });
        stmts.push(format!("-- {};", designer::drop_sql(target_schema, &t.name, "table", false)?));
    }
    let sql = if stmts.is_empty() {
        String::new()
    } else {
        // FK checks off: new tables may reference each other in any order.
        format!("SET FOREIGN_KEY_CHECKS = 0;\n\n{}\n\nSET FOREIGN_KEY_CHECKS = 1;\n", stmts.join("\n\n"))
    };
    Ok(Comparison { diffs, sql })
}

/// Every base table of a schema, fully loaded (sorted by name, as the tree lists them).
// ponytail: one designer::load per table (a handful of queries each); batch per schema if hundreds of tables get slow.
pub async fn load_tables(pool: &Pool, schema: &str, mariadb: bool) -> AppResult<Vec<TableDef>> {
    let mut out = Vec::new();
    for o in browse::objects(pool, schema).await?.into_iter().filter(|o| o.kind == "table") {
        out.push(designer::load(pool, schema, &o.name, mariadb).await?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::designer::{ColumnDef, DefaultValue, IndexColumn, IndexDef};

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

    fn table(name: &str, cols: &[(&str, &str)]) -> TableDef {
        TableDef {
            name: name.into(),
            engine: "InnoDB".into(),
            collation: "utf8mb4_general_ci".into(),
            comment: String::new(),
            columns: cols.iter().map(|(n, t)| col(n, t)).collect(),
            indexes: vec![IndexDef {
                original: Some("PRIMARY".into()),
                name: "PRIMARY".into(),
                kind: "PRIMARY".into(),
                columns: vec![IndexColumn { name: cols[0].0.into(), length: None, desc: false }],
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
    fn same_schemas_give_no_script() {
        let a = [table("a", &[("id", "int(11)")])];
        let c = compare("prod", &a, &a, true).unwrap();
        assert!(c.diffs.is_empty());
        assert_eq!(c.sql, "");
    }

    #[test]
    fn creates_alters_and_comments_drops() {
        let src = [table("a", &[("id", "int(11)"), ("name", "varchar(50)")]), table("new_t", &[("id", "bigint(20)")])];
        let tgt = [table("a", &[("id", "int(11)")]), table("old_t", &[("id", "int(11)")])];
        let c = compare("prod", &src, &tgt, true).unwrap();
        let st: Vec<_> = c.diffs.iter().map(|d| (d.name.as_str(), d.status)).collect();
        assert_eq!(st, [("a", "alter"), ("new_t", "create"), ("old_t", "drop")]);
        assert!(c.sql.starts_with("SET FOREIGN_KEY_CHECKS = 0;"));
        assert!(c.sql.contains("ALTER TABLE `prod`.`a`\n  ADD COLUMN `name` varchar(50) NOT NULL AFTER `id`;"), "{}", c.sql);
        assert!(c.sql.contains("CREATE TABLE `prod`.`new_t` ("), "{}", c.sql);
        assert!(c.sql.contains("-- DROP TABLE `prod`.`old_t`;"), "{}", c.sql);
        assert!(c.sql.trim_end().ends_with("SET FOREIGN_KEY_CHECKS = 1;"));
    }

    // Live: OTARIA_TEST_DB="host:port:user:password", e.g. cargo test -- --ignored compare_on_test_server
    #[tokio::test]
    #[ignore]
    async fn compare_on_test_server() {
        use mysql_async::prelude::Queryable;
        let v = std::env::var("OTARIA_TEST_DB").expect("OTARIA_TEST_DB");
        let p: Vec<&str> = v.splitn(4, ':').collect();
        let opts = mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false);
        let pool = Pool::new(opts);
        let mut conn = pool.get_conn().await.unwrap();
        conn.query_drop(
            "DROP DATABASE IF EXISTS cmp_src; DROP DATABASE IF EXISTS cmp_tgt; CREATE DATABASE cmp_src; CREATE DATABASE cmp_tgt;
             CREATE TABLE cmp_src.parent (id INT PRIMARY KEY, name VARCHAR(20) NOT NULL);
             CREATE TABLE cmp_src.child (id INT PRIMARY KEY, parent_id INT, KEY (parent_id), FOREIGN KEY (parent_id) REFERENCES cmp_src.parent (id));
             CREATE TABLE cmp_tgt.parent (id INT PRIMARY KEY);
             CREATE TABLE cmp_tgt.gone (id INT PRIMARY KEY);",
        )
        .await
        .unwrap();
        let src = load_tables(&pool, "cmp_src", true).await.unwrap();
        let tgt = load_tables(&pool, "cmp_tgt", true).await.unwrap();
        let c = compare("cmp_tgt", &src, &tgt, true).unwrap();
        assert!(c.sql.contains("FOREIGN KEY (`parent_id`) REFERENCES `parent` (`id`)"), "{}", c.sql);
        // Run the script (comments skipped), then the target must match the source but for `gone`.
        conn.query_drop(&c.sql).await.unwrap();
        let after = load_tables(&pool, "cmp_tgt", true).await.unwrap();
        let again = compare("cmp_tgt", &src, &after, true).unwrap();
        assert_eq!(again.diffs, vec![TableDiff { name: "gone".into(), status: "drop" }], "{}", again.sql);
        conn.query_drop("DROP DATABASE cmp_src; DROP DATABASE cmp_tgt").await.unwrap();
    }
}
