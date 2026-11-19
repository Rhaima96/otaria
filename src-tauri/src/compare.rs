//! Schema comparison: a source schema against a target schema (same or another connection), and
//! the script that turns the target into the source.
//!
//! Tables reuse the designer's loader and diff: both sides load with `original = name`, so columns,
//! indexes and constraints match by name (a renamed column shows as drop + add). Views, routines,
//! triggers and events compare their `SHOW CREATE` text, read after `USE schema` (no schema name
//! in it) and without DEFINER (accounts differ between servers). Sequences (MariaDB) compare their
//! `SHOW CREATE SEQUENCE` text too, and change through ALTER SEQUENCE, which keeps the current value.

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
    /// sequence | table | function | procedure | view | trigger | event
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

/// A sequence, view, routine, trigger or event: its `SHOW CREATE` text, normalized by `load`.
#[derive(Debug, Clone)]
pub struct Code {
    pub kind: String,
    pub name: String,
    pub ddl: String,
}

pub struct Schema {
    pub tables: Vec<TableDef>,
    pub code: Vec<Code>,
    /// The schema's default collation: a table on it took the server's default, not a choice.
    pub collation: String,
}

/// Script order: functions before the views and procedures that may call them, triggers and events last.
/// Views among themselves: `load` puts each after the views it reads.
const CODE_ORDER: [&str; 5] = ["function", "procedure", "view", "trigger", "event"];

/// Diff of two loaded schemas. Every DROP comes out commented: deleting is left to the user.
pub fn compare(target_schema: &str, src: &Schema, tgt: &Schema, mariadb: bool) -> AppResult<Comparison> {
    let mut diffs = Vec::new();
    let mut stmts = Vec::new();
    let diff = |name: &str, kind: &str, status| ObjectDiff { name: name.into(), kind: kind.into(), status };
    // Sequences first: tables may take their DEFAULT from NEXTVAL(seq).
    let seqs = |v: &Schema| v.code.iter().filter(|c| c.kind == "sequence").cloned().collect::<Vec<_>>();
    let (src_q, tgt_q) = (seqs(src), seqs(tgt));
    for s in &src_q {
        match tgt_q.iter().find(|t| t.name == s.name) {
            None => {
                diffs.push(diff(&s.name, "sequence", "create"));
                stmts.push(format!("{};", s.ddl));
            }
            Some(t) if t.ddl != s.ddl => {
                diffs.push(diff(&s.name, "sequence", "alter"));
                stmts.extend(alter_sequence(&s.name, &s.ddl, &t.ddl));
            }
            Some(_) => {}
        }
    }
    for t in tgt_q.iter().filter(|t| !src_q.iter().any(|s| s.name == t.name)) {
        diffs.push(diff(&t.name, "sequence", "drop"));
        stmts.push(format!("-- {};", designer::drop_sql(target_schema, &t.name, "sequence", false)?));
    }
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
                    let other = t.columns.iter().find(|o| o.name == c.name);
                    c.original = other.map(|_| c.name.clone());
                    // int(11) on MariaDB, int on MySQL 8: same type, written two ways.
                    if let Some(o) = other.filter(|o| int_width_free(&o.col_type) == int_width_free(&c.col_type)) {
                        c.col_type = o.col_type.clone();
                    }
                }
                // Both tables on their schema's default collation (each server's own): not a difference.
                if s.collation == src.collation && t.collation == tgt.collation {
                    s.collation = t.collation.clone();
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
                Some(t) if comparable(kind, &t.ddl) != comparable(kind, &s.ddl) => "alter",
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
        format!(
            "USE {};\nSET FOREIGN_KEY_CHECKS = 0;\n{};\n\n{}\n\n{};\nSET FOREIGN_KEY_CHECKS = 1;\n",
            ident(target_schema),
            designer::ESCAPES_ON,
            stmts.join("\n\n"),
            designer::ESCAPES_RESTORE
        )
    };
    Ok(Comparison { diffs, sql })
}

/// `CREATE SEQUENCE `s` [as int] start with … nocycle ENGINE=…` → ("as int", "start with … nocycle").
fn sequence_parts<'a>(name: &str, ddl: &'a str) -> Option<(&'a str, &'a str)> {
    let rest = ddl.strip_prefix(&format!("CREATE SEQUENCE {} ", ident(name)))?;
    let rest = rest.split(" ENGINE=").next()?.trim();
    let i = rest.find("start with")?;
    Some((rest[..i].trim(), &rest[i..]))
}

/// ALTER SEQUENCE keeps the current value (CREATE OR REPLACE would restart it). MariaDB refuses AS <type>
/// together with other options, so the type change is its own statement: first when maxvalue grows
/// (wider type), after the options when it shrinks (the new maxvalue must fit the narrower type).
// ponytail: engine changes are not carried over.
fn alter_sequence(name: &str, src: &str, tgt: &str) -> Vec<String> {
    let (Some((src_as, src_opts)), Some((tgt_as, tgt_opts))) = (sequence_parts(name, src), sequence_parts(name, tgt)) else {
        // Unknown shape: offer the replacement, commented, since it restarts the sequence.
        return vec![format!("-- CREATE OR REPLACE restarts the sequence:\n-- {};", src.replacen("CREATE ", "CREATE OR REPLACE ", 1))];
    };
    let ty = (src_as != tgt_as).then(|| format!("ALTER SEQUENCE {} {};", ident(name), if src_as.is_empty() { "as bigint" } else { src_as }));
    let opts = (src_opts != tgt_opts).then(|| format!("ALTER SEQUENCE {} {src_opts};", ident(name)));
    let max = |o: &str| o.split_once("maxvalue ").and_then(|(_, r)| r.split(' ').next()?.parse::<i128>().ok());
    let shrinks = matches!((max(src_opts), max(tgt_opts)), (Some(s), Some(t)) if s < t);
    let order = if shrinks { [opts, ty] } else { [ty, opts] };
    order.into_iter().flatten().collect()
}

/// Integer type without its display width (`int(11) unsigned` → `int unsigned`). tinyint(1) keeps it:
/// MySQL 8 still writes it (it means boolean there).
fn int_width_free(ty: &str) -> String {
    let lower = ty.to_ascii_lowercase();
    let is_int = ["tinyint(", "smallint(", "mediumint(", "int(", "bigint("].iter().any(|p| lower.starts_with(p));
    match lower.find('(').zip(lower.find(')')) {
        Some((open, close)) if is_int && !lower.starts_with("tinyint(1)") => format!("{}{}", &lower[..open], &lower[close + 1..]),
        _ => lower,
    }
}

/// Text compared for code objects. An event's STARTS / ENDS hold the moment it was created on each
/// server: left out, or every event would show as changed.
fn comparable(kind: &str, ddl: &str) -> String {
    if kind != "event" {
        return ddl.to_string();
    }
    let mut out = ddl.to_string();
    for kw in [" STARTS '", " ENDS '"] {
        if let Some(i) = out.find(kw) {
            let end = out[i + kw.len()..].find('\'').map_or(out.len(), |e| i + kw.len() + e + 1);
            out.replace_range(i..end, "");
        }
    }
    out
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
/// Text inside string literals and backtick names is left alone.
fn strip_schema(ddl: &str, schema: &str) -> String {
    let quoted = format!("{}.", ident(schema));
    let plain = format!("{schema}.");
    let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '`' | '.');
    let mut out = String::with_capacity(ddl.len());
    let mut quote: Option<char> = None;
    let mut i = 0;
    while let Some(c) = ddl[i..].chars().next() {
        let rest = &ddl[i..];
        match quote {
            Some(q) if q != '`' && c == '\\' => {
                // Escaped character: copy it with its backslash.
                let n = 1 + rest[1..].chars().next().map_or(0, char::len_utf8);
                out.push_str(&rest[..n]);
                i += n;
                continue;
            }
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if rest.starts_with(&quoted) => {
                i += quoted.len();
                continue;
            }
            None if rest.starts_with(&plain) && !out.chars().next_back().is_some_and(word) => {
                i += plain.len();
                continue;
            }
            None if matches!(c, '\'' | '"' | '`') => quote = Some(c),
            None => {}
        }
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Every table (fully loaded), sequence, view, routine, trigger and event of a schema.
pub async fn load(pool: &Pool, schema: &str, mariadb: bool) -> AppResult<Schema> {
    let objects = browse::objects(pool, schema).await?;
    let tables = designer::load_tables(pool, schema, None, mariadb).await?;
    let mut conn = pool.get_conn().await?;
    // After USE, SHOW CREATE leaves the schema name out of view bodies.
    conn.query_drop(format!("USE {}", ident(schema))).await?;
    let mut code = Vec::new();
    for o in objects.iter().filter(|o| o.kind == "sequence" || CODE_ORDER.contains(&o.kind.as_str())) {
        let ddl = browse::ddl_on(&mut conn, schema, &o.name, &o.kind).await?;
        code.push(Code { kind: o.kind.clone(), name: o.name.clone(), ddl: strip_schema(&strip_definer(&ddl), schema) });
    }
    let (views, mut code): (Vec<Code>, Vec<Code>) = code.into_iter().partition(|c| c.kind == "view");
    let pairs: Vec<(String, String)> = views.iter().map(|v| (v.name.clone(), v.ddl.clone())).collect();
    code.extend(browse::view_order(&pairs).into_iter().map(|i| views[i].clone()));
    let collation: String = conn
        .exec_first("SELECT DEFAULT_COLLATION_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?", (schema,))
        .await?
        .unwrap_or_default();
    Ok(Schema { tables, code, collation })
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
        Schema {
            tables,
            code: code.iter().map(|(k, n, d)| Code { kind: k.to_string(), name: n.to_string(), ddl: d.to_string() }).collect(),
            collation: "utf8mb4_general_ci".into(),
        }
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
        assert!(c.sql.contains(designer::ESCAPES_ON) && c.sql.contains(designer::ESCAPES_RESTORE));
        assert!(c.sql.contains("-- DROP FUNCTION `prod`.`f`;"), "{}", c.sql);
        assert!(c.sql.contains("DROP PROCEDURE IF EXISTS `prod`.`p`;\nDELIMITER $$\nCREATE PROCEDURE `p`()\nBEGIN SELECT 1; END$$\nDELIMITER ;"), "{}", c.sql);
        assert!(c.sql.contains("CREATE OR REPLACE ALGORITHM=UNDEFINED SQL SECURITY DEFINER VIEW `v` AS select 2;"), "{}", c.sql);
        assert!(c.sql.contains("ALTER TABLE `prod`.`a`\n  ADD COLUMN `name` varchar(50) NOT NULL AFTER `id`;"), "{}", c.sql);
        assert!(c.sql.contains("CREATE TABLE `prod`.`new_t` ("), "{}", c.sql);
        assert!(c.sql.contains("-- DROP TABLE `prod`.`old_t`;"), "{}", c.sql);
        assert!(c.sql.trim_end().ends_with("SET FOREIGN_KEY_CHECKS = 1;"));
    }

    #[test]
    fn sequences_alter_without_restart() {
        fn seq(name: &str, opts: &str) -> Code {
            Code { kind: "sequence".into(), name: name.into(), ddl: format!("CREATE SEQUENCE `{name}` {opts} ENGINE=InnoDB") }
        }
        let opts = "start with 1 minvalue 1 maxvalue 9 increment by 1 cache 1000 nocycle";
        let new = seq("n", opts);
        let src = Schema {
            tables: vec![],
            code: vec![seq("a", "start with 1 minvalue 1 maxvalue 99 increment by 2 cache 1000 nocycle"), seq("b", &format!("as int {opts}")), new.clone()],
            collation: String::new(),
        };
        let tgt = Schema {
            tables: vec![],
            code: vec![seq("a", "start with 1 minvalue 1 maxvalue 99 increment by 1 cache 1000 nocycle"), seq("b", opts), seq("o", opts)],
            collation: String::new(),
        };
        let c = compare("prod", &src, &tgt, true).unwrap();
        let st: Vec<_> = c.diffs.iter().map(|d| (d.name.as_str(), d.status)).collect();
        assert_eq!(st, [("a", "alter"), ("b", "alter"), ("n", "create"), ("o", "drop")]);
        assert!(c.sql.contains("ALTER SEQUENCE `a` start with 1 minvalue 1 maxvalue 99 increment by 2 cache 1000 nocycle;"), "{}", c.sql);
        assert!(c.sql.contains("ALTER SEQUENCE `b` as int;") && !c.sql.contains("ALTER SEQUENCE `b` start"), "{}", c.sql);
        // Narrower type: the smaller maxvalue goes first, then the type; wider: the type first.
        let big = "CREATE SEQUENCE `s` start with 1 minvalue 1 maxvalue 9223372036854775806 increment by 1 cache 1000 nocycle ENGINE=InnoDB";
        let int = "CREATE SEQUENCE `s` as int start with 1 minvalue 1 maxvalue 2147483646 increment by 1 cache 1000 nocycle ENGINE=InnoDB";
        let narrow = alter_sequence("s", int, big);
        assert!(narrow[0].contains("maxvalue 2147483646") && narrow[1] == "ALTER SEQUENCE `s` as int;", "{narrow:?}");
        let wide = alter_sequence("s", big, int);
        assert!(wide[0] == "ALTER SEQUENCE `s` as bigint;" && wide[1].contains("maxvalue 9223372036854775806"), "{wide:?}");
        assert!(c.sql.contains(&format!("{};", new.ddl)) && c.sql.contains("-- DROP SEQUENCE `prod`.`o`;"), "{}", c.sql);
        // Before any table: a table DEFAULT may call NEXTVAL on it.
        assert!(c.sql.find("SEQUENCE `a`").unwrap() < c.sql.find("SET FOREIGN_KEY_CHECKS = 1").unwrap());
        assert_eq!(alter_sequence("x", "weird", "other")[0].lines().count(), 2);
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
        // Inside a string or a backtick name the text is data, kept as is.
        assert_eq!(strip_schema("select 'src.x', \"it\\'s src.y\", `a src.z` from src.t", "src"), "select 'src.x', \"it\\'s src.y\", `a src.z` from t");
    }

    #[test]
    fn ignores_spelling_differences_between_servers() {
        assert_eq!(int_width_free("int(11) unsigned"), "int unsigned");
        assert_eq!(int_width_free("BIGINT(20)"), "bigint");
        assert_eq!(int_width_free("tinyint(1)"), "tinyint(1)");
        assert_eq!(int_width_free("varchar(10)"), "varchar(10)");
        // MariaDB vs MySQL 8 spelling, each table on its own server's default collation: no difference.
        let src = schema(vec![table("a", &[("id", "int(11)"), ("n", "bigint(20) unsigned")])], &[]);
        let mut tgt = schema(vec![table("a", &[("id", "int"), ("n", "bigint unsigned")])], &[]);
        tgt.collation = "utf8mb4_0900_ai_ci".into();
        tgt.tables[0].collation = "utf8mb4_0900_ai_ci".into();
        assert!(compare("prod", &src, &tgt, true).unwrap().diffs.is_empty());
        // Events created at different moments only differ by STARTS.
        let ev = |at: &str| format!("CREATE EVENT `e` ON SCHEDULE EVERY 1 DAY STARTS '{at}' ON COMPLETION NOT PRESERVE ENABLE DO select 1");
        let a = schema(vec![], &[("event", "e", &ev("2026-01-01 00:00:00"))]);
        let b = schema(vec![], &[("event", "e", &ev("2026-11-19 10:00:00"))]);
        assert!(compare("prod", &a, &b, true).unwrap().diffs.is_empty());
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
             CREATE VIEW cmp_src.a_top AS SELECT id FROM cmp_src.v_parent;
             CREATE PROCEDURE cmp_src.hello() BEGIN SELECT 'hi'; SELECT 'there'; END;
             CREATE PROCEDURE cmp_tgt.hello() BEGIN SELECT 'old'; END;
             CREATE TRIGGER cmp_src.child_bi BEFORE INSERT ON cmp_src.child FOR EACH ROW SET NEW.id = NEW.id;
             CREATE FUNCTION cmp_tgt.extra() RETURNS INT RETURN 1;
             CREATE SEQUENCE cmp_src.seq_a INCREMENT BY 5; CREATE SEQUENCE cmp_tgt.seq_a;
             SELECT NEXTVAL(cmp_tgt.seq_a), NEXTVAL(cmp_tgt.seq_a), NEXTVAL(cmp_tgt.seq_a);
             CREATE SEQUENCE cmp_src.seq_b AS INT; CREATE SEQUENCE cmp_tgt.seq_b;
             CREATE SEQUENCE cmp_src.seq_new; CREATE SEQUENCE cmp_tgt.seq_old;",
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
        assert_eq!(st, [("seq_old", "drop"), ("gone", "drop"), ("extra", "drop")], "{}", again.sql);
        // ALTER SEQUENCE kept the target's position (3 used), with the source's increment.
        let next: Option<i64> = conn.query_first("SELECT NEXTVAL(cmp_tgt.seq_a)").await.unwrap();
        assert!(next.unwrap() > 3, "{next:?}");
        conn.query_drop("DROP DATABASE cmp_src; DROP DATABASE cmp_tgt").await.unwrap();
    }
}
