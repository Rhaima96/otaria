//! Schema comparison: a source schema against a target schema (same or another connection), and
//! the script that turns the target into the source.
//!
//! Tables reuse the designer's loader and diff: both sides load with `original = name`, so columns,
//! indexes and constraints match by name (a renamed column shows as drop + add). Views, routines,
//! triggers and events compare their `SHOW CREATE` text, read after `USE schema` (no schema name
//! in it) and without DEFINER (accounts differ between servers).

use mysql_async::prelude::Queryable;
use mysql_async::Pool;
use serde::Serialize;

use crate::browse::{self, ident};
use crate::designer::{self, Algorithm, TableDef};
use crate::error::AppResult;

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDiff {
    pub name: String,
    /// table | function | procedure | view | trigger | event
    pub kind: String,
    /// create (only in source) | alter (differs) | drop (only in target)
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    pub diffs: Vec<ObjectDiff>,
    /// Empty when nothing differs.
    pub sql: String,
}

/// A view, routine, trigger or event: its `SHOW CREATE` text, normalized by `load`.
#[derive(Debug, Clone)]
pub struct Code {
    pub kind: String,
    pub name: String,
    pub ddl: String,
}

pub struct Schema {
    pub tables: Vec<TableDef>,
    pub code: Vec<Code>,
}

/// Script order: functions before the views and procedures that may call them, triggers and events last.
// ponytail: a view built on another view comes in name order; reorder by hand if CREATE fails on it.
const CODE_ORDER: [&str; 5] = ["function", "procedure", "view", "trigger", "event"];

/// Diff of two loaded schemas. Every DROP comes out commented: deleting is left to the user.
pub fn compare(target_schema: &str, src: &Schema, tgt: &Schema, mariadb: bool) -> AppResult<Comparison> {
    let mut diffs = Vec::new();
    let mut stmts = Vec::new();
    let diff = |name: &str, kind: &str, status| ObjectDiff { name: name.into(), kind: kind.into(), status };
    let (src_t, tgt_t) = (&src.tables, &tgt.tables);
    for s in src_t {
        match tgt_t.iter().find(|t| t.name == s.name) {
            None => {
                diffs.push(diff(&s.name, "table", "create"));
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
                    diffs.push(diff(&s.name, "table", "alter"));
                    stmts.extend(alter.into_iter().map(|a| format!("{a};")));
                }
            }
        }
    }
    for t in tgt_t.iter().filter(|t| !src_t.iter().any(|s| s.name == t.name)) {
        diffs.push(diff(&t.name, "table", "drop"));
        stmts.push(format!("-- {};", designer::drop_sql(target_schema, &t.name, "table", false)?));
    }
    for kind in CODE_ORDER {
        let of = |v: &Schema| v.code.iter().filter(|c| c.kind == kind).cloned().collect::<Vec<_>>();
        let (src_c, tgt_c) = (of(src), of(tgt));
        for s in &src_c {
            let status = match tgt_c.iter().find(|t| t.name == s.name) {
                None => "create",
                Some(t) if t.ddl != s.ddl => "alter",
                Some(_) => continue,
            };
            diffs.push(diff(&s.name, kind, status));
            stmts.push(create_code(target_schema, s));
        }
        for t in tgt_c.iter().filter(|t| !src_c.iter().any(|s| s.name == t.name)) {
            diffs.push(diff(&t.name, kind, "drop"));
            stmts.push(format!("-- {};", designer::drop_sql(target_schema, &t.name, kind, false)?));
        }
    }
    let sql = if stmts.is_empty() {
        String::new()
    } else {
        // USE: views and routines are created without a schema name. FK checks off: new tables may
        // reference each other in any order.
        format!("USE {};\nSET FOREIGN_KEY_CHECKS = 0;\n\n{}\n\nSET FOREIGN_KEY_CHECKS = 1;\n", ident(target_schema), stmts.join("\n\n"))
    };
    Ok(Comparison { diffs, sql })
}

/// Views: CREATE OR REPLACE. Routines, triggers, events: DROP IF EXISTS then CREATE, in a DELIMITER
/// block for their body (the SQL editor understands DELIMITER).
fn create_code(target_schema: &str, c: &Code) -> String {
    if c.kind == "view" {
        return format!("{};", c.ddl.replacen("CREATE ", "CREATE OR REPLACE ", 1));
    }
    format!(
        "DROP {} IF EXISTS {}.{};\nDELIMITER $$\n{}$$\nDELIMITER ;",
        c.kind.to_uppercase(),
        ident(target_schema),
        ident(&c.name),
        c.ddl
    )
}

/// `DEFINER=`user`@`host`` (or CURRENT_USER) removed from a CREATE header: the account running
/// the script becomes the definer.
fn strip_definer(ddl: &str) -> String {
    let Some(i) = ddl.find(" DEFINER=") else { return ddl.to_string() };
    let rest = &ddl[i + 9..];
    // A backtick-quoted name ends at a backtick that is not doubled.
    let quoted = |s: &str| -> usize {
        let b = s.as_bytes();
        let mut j = 1;
        while j < b.len() {
            if b[j] == b'`' {
                if b.get(j + 1) == Some(&b'`') {
                    j += 2;
                    continue;
                }
                return j + 1;
            }
            j += 1;
        }
        b.len()
    };
    let end = if rest.starts_with('`') {
        let user = quoted(rest);
        match rest[user..].strip_prefix('@') {
            Some(h) if h.starts_with('`') => user + 1 + quoted(h),
            _ => user,
        }
    } else {
        rest.find(char::is_whitespace).unwrap_or(rest.len())
    };
    format!("{}{}", &ddl[..i], &rest[end..])
}

/// The schema's own name removed where it qualifies a name (`schema`.x or schema.x): a view still
/// qualifies stored function calls after USE, and a trigger keeps the text it was created with.
// ponytail: plain text scan; `schema.` inside a string literal of a body is stripped too.
fn strip_schema(ddl: &str, schema: &str) -> String {
    let quoted = format!("{}.", ident(schema));
    let mut out = ddl.replace(&quoted, "");
    let plain = format!("{schema}.");
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$' || c == '`' || c == '.';
    let mut from = 0;
    while let Some(i) = out[from..].find(&plain).map(|i| from + i) {
        if out[..i].chars().next_back().is_some_and(word) {
            from = i + plain.len();
        } else {
            out.replace_range(i..i + plain.len(), "");
            from = i;
        }
    }
    out
}

/// Every table (fully loaded), view, routine, trigger and event of a schema. Sequences are not compared.
// ponytail: one designer::load per table (a handful of queries each); batch per schema if hundreds of tables get slow.
pub async fn load(pool: &Pool, schema: &str, mariadb: bool) -> AppResult<Schema> {
    let objects = browse::objects(pool, schema).await?;
    let mut tables = Vec::new();
    for o in objects.iter().filter(|o| o.kind == "table") {
        tables.push(designer::load(pool, schema, &o.name, mariadb).await?);
    }
    let mut conn = pool.get_conn().await?;
    // After USE, SHOW CREATE leaves the schema name out of view bodies.
    conn.query_drop(format!("USE {}", ident(schema))).await?;
    let mut code = Vec::new();
    for o in objects.iter().filter(|o| CODE_ORDER.contains(&o.kind.as_str())) {
        let ddl = browse::ddl_on(&mut conn, schema, &o.name, &o.kind).await?;
        code.push(Code { kind: o.kind.clone(), name: o.name.clone(), ddl: strip_schema(&strip_definer(&ddl), schema) });
    }
    Ok(Schema { tables, code })
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

    fn schema(tables: Vec<TableDef>, code: &[(&str, &str, &str)]) -> Schema {
        Schema { tables, code: code.iter().map(|(k, n, d)| Code { kind: k.to_string(), name: n.to_string(), ddl: d.to_string() }).collect() }
    }

    #[test]
    fn same_schemas_give_no_script() {
        let a = schema(vec![table("a", &[("id", "int(11)")])], &[("view", "v", "CREATE VIEW `v` AS select 1")]);
        let c = compare("prod", &a, &a, true).unwrap();
        assert!(c.diffs.is_empty());
        assert_eq!(c.sql, "");
    }

    #[test]
    fn creates_alters_and_comments_drops() {
        let src = schema(
            vec![table("a", &[("id", "int(11)"), ("name", "varchar(50)")]), table("new_t", &[("id", "bigint(20)")])],
            &[("view", "v", "CREATE ALGORITHM=UNDEFINED SQL SECURITY DEFINER VIEW `v` AS select 2"), ("procedure", "p", "CREATE PROCEDURE `p`()\nBEGIN SELECT 1; END")],
        );
        let tgt = schema(
            vec![table("a", &[("id", "int(11)")]), table("old_t", &[("id", "int(11)")])],
            &[("view", "v", "CREATE ALGORITHM=UNDEFINED SQL SECURITY DEFINER VIEW `v` AS select 1"), ("function", "f", "CREATE FUNCTION `f`() RETURNS int(11) RETURN 1")],
        );
        let c = compare("prod", &src, &tgt, true).unwrap();
        let st: Vec<_> = c.diffs.iter().map(|d| (d.name.as_str(), d.kind.as_str(), d.status)).collect();
        assert_eq!(
            st,
            [("a", "table", "alter"), ("new_t", "table", "create"), ("old_t", "table", "drop"), ("f", "function", "drop"), ("p", "procedure", "create"), ("v", "view", "alter")]
        );
        assert!(c.sql.starts_with("USE `prod`;\nSET FOREIGN_KEY_CHECKS = 0;"));
        assert!(c.sql.contains("-- DROP FUNCTION `prod`.`f`;"), "{}", c.sql);
        assert!(c.sql.contains("DROP PROCEDURE IF EXISTS `prod`.`p`;\nDELIMITER $$\nCREATE PROCEDURE `p`()\nBEGIN SELECT 1; END$$\nDELIMITER ;"), "{}", c.sql);
        assert!(c.sql.contains("CREATE OR REPLACE ALGORITHM=UNDEFINED SQL SECURITY DEFINER VIEW `v` AS select 2;"), "{}", c.sql);
        assert!(c.sql.contains("ALTER TABLE `prod`.`a`\n  ADD COLUMN `name` varchar(50) NOT NULL AFTER `id`;"), "{}", c.sql);
        assert!(c.sql.contains("CREATE TABLE `prod`.`new_t` ("), "{}", c.sql);
        assert!(c.sql.contains("-- DROP TABLE `prod`.`old_t`;"), "{}", c.sql);
        assert!(c.sql.trim_end().ends_with("SET FOREIGN_KEY_CHECKS = 1;"));
    }

    #[test]
    fn strips_definer() {
        assert_eq!(strip_definer("CREATE DEFINER=`root`@`%` PROCEDURE `p`() SELECT 1"), "CREATE PROCEDURE `p`() SELECT 1");
        assert_eq!(
            strip_definer("CREATE ALGORITHM=UNDEFINED DEFINER=`a``b`@`local host` SQL SECURITY DEFINER VIEW `v` AS select 1"),
            "CREATE ALGORITHM=UNDEFINED SQL SECURITY DEFINER VIEW `v` AS select 1"
        );
        assert_eq!(strip_definer("CREATE DEFINER=CURRENT_USER TRIGGER t"), "CREATE TRIGGER t");
        assert_eq!(strip_definer("CREATE VIEW `v` AS select 1"), "CREATE VIEW `v` AS select 1");
    }

    #[test]
    fn strips_own_schema() {
        assert_eq!(strip_schema("select `src`.`f`(`t`.`id`) from `t`", "src"), "select `f`(`t`.`id`) from `t`");
        assert_eq!(strip_schema("CREATE TRIGGER src.t BEFORE INSERT ON src.c", "src"), "CREATE TRIGGER t BEFORE INSERT ON c");
        // Only a whole name: mysrc.x, other.src.x and `a`.src stay.
        assert_eq!(strip_schema("mysrc.x, other.src.x, `other`.`t`", "src"), "mysrc.x, other.src.x, `other`.`t`");
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
             CREATE TABLE cmp_tgt.gone (id INT PRIMARY KEY);
             CREATE FUNCTION cmp_src.twice(x INT) RETURNS INT DETERMINISTIC RETURN x * 2;
             CREATE VIEW cmp_src.v_parent AS SELECT id, cmp_src.twice(id) AS t FROM cmp_src.parent;
             CREATE PROCEDURE cmp_src.hello() BEGIN SELECT 'hi'; SELECT 'there'; END;
             CREATE PROCEDURE cmp_tgt.hello() BEGIN SELECT 'old'; END;
             CREATE TRIGGER cmp_src.child_bi BEFORE INSERT ON cmp_src.child FOR EACH ROW SET NEW.id = NEW.id;
             CREATE FUNCTION cmp_tgt.extra() RETURNS INT RETURN 1;",
        )
        .await
        .unwrap();
        let src = load(&pool, "cmp_src", true).await.unwrap();
        let tgt = load(&pool, "cmp_tgt", true).await.unwrap();
        let c = compare("cmp_tgt", &src, &tgt, true).unwrap();
        assert!(c.sql.contains("FOREIGN KEY (`parent_id`) REFERENCES `parent` (`id`)"), "{}", c.sql);
        assert!(!c.sql.contains("cmp_src") && !c.sql.contains("DEFINER="), "{}", c.sql);
        // Run the script as the SQL editor does (DELIMITER split, comments skipped); the target must
        // then match the source but for the commented drops.
        for stmt in crate::query::pick(&c.sql, None) {
            conn.query_drop(&stmt).await.unwrap_or_else(|e| panic!("{stmt}: {e}"));
        }
        let after = load(&pool, "cmp_tgt", true).await.unwrap();
        let again = compare("cmp_tgt", &src, &after, true).unwrap();
        let st: Vec<_> = again.diffs.iter().map(|d| (d.name.as_str(), d.status)).collect();
        assert_eq!(st, [("gone", "drop"), ("extra", "drop")], "{}", again.sql);
        conn.query_drop("DROP DATABASE cmp_src; DROP DATABASE cmp_tgt").await.unwrap();
    }
}
