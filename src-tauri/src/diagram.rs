//! ER diagram of a schema as Mermaid `erDiagram` text (phase 7b.1), built on demand from
//! information_schema. GitHub, GitLab, Notion and Obsidian render it as is.

use std::collections::BTreeSet;
use std::path::Path;

use mysql_async::Pool;
use mysql_async::prelude::Queryable;

use crate::error::{AppError, AppResult};

pub struct Column {
    pub table: String,
    pub name: String,
    /// DATA_TYPE (`varchar`, not `varchar(255)`): Mermaid types cannot hold a comma.
    pub data_type: String,
    /// COLUMN_KEY: PRI | UNI | MUL | ''.
    pub key: String,
    pub nullable: bool,
}

/// One foreign key; `parent` is None when it points to another schema.
pub struct Fk {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
    pub parent: Option<String>,
}

pub async fn load(pool: &Pool, schema: &str) -> AppResult<(Vec<Column>, Vec<Fk>)> {
    let mut conn = pool.get_conn().await?;
    let columns = conn
        .exec_map(
            "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE, COLUMN_KEY, IS_NULLABLE = 'YES'
             FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME, ORDINAL_POSITION",
            (schema,),
            |(table, name, data_type, key, nullable)| Column { table, name, data_type, key, nullable },
        )
        .await?;
    let rows: Vec<(String, String, String, String, String)> = conn
        .exec(
            "SELECT TABLE_NAME, CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME
             FROM information_schema.KEY_COLUMN_USAGE
             WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL
             ORDER BY TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION",
            (schema,),
        )
        .await?;
    let mut fks: Vec<Fk> = Vec::new();
    for (table, name, column, ref_schema, ref_table) in rows {
        match fks.last_mut() {
            Some(f) if f.table == table && f.name == name => f.columns.push(column),
            _ => fks.push(Fk { name, table, columns: vec![column], parent: (ref_schema == schema).then_some(ref_table) }),
        }
    }
    Ok((columns, fks))
}

/// `neighbors`: also draw the tables one foreign key away from the selected ones.
pub fn render(columns: &[Column], fks: &[Fk], selected: &[String], neighbors: bool) -> String {
    let mut tables: BTreeSet<&str> = selected.iter().map(String::as_str).collect();
    if neighbors {
        for f in fks {
            if let Some(p) = &f.parent {
                if selected.contains(&f.table) {
                    tables.insert(p);
                }
                if selected.contains(p) {
                    tables.insert(&f.table);
                }
            }
        }
    }
    let in_fk = |t: &str, c: &str| fks.iter().any(|f| f.table == t && f.columns.iter().any(|x| x == c));

    let mut out = String::from("erDiagram\n");
    let mut current = "";
    for c in columns.iter().filter(|c| tables.contains(c.table.as_str())) {
        if c.table != current {
            if !current.is_empty() {
                out.push_str("    }\n");
            }
            current = &c.table;
            out.push_str(&format!("    {} {{\n", entity(current)));
        }
        let keys: Vec<&str> = [
            (c.key == "PRI").then_some("PK"),
            in_fk(&c.table, &c.name).then_some("FK"),
            (c.key == "UNI").then_some("UK"),
        ]
        .into_iter()
        .flatten()
        .collect();
        out.push_str(format!("        {} {} {}", word(&c.data_type), word(&c.name), keys.join(", ")).trim_end());
        out.push('\n');
    }
    if !current.is_empty() {
        out.push_str("    }\n");
    }

    for f in fks {
        let Some(parent) = f.parent.as_deref().filter(|p| tables.contains(p) && tables.contains(f.table.as_str())) else { continue };
        let col = |name: &str| columns.iter().find(|c| c.table == f.table && c.name == name);
        // Any nullable FK column = the parent is optional. A one-column FK that is also unique = one-to-one.
        let optional = f.columns.iter().any(|n| col(n).is_none_or(|c| c.nullable));
        let unique = f.columns.len() == 1 && col(&f.columns[0]).is_some_and(|c| c.key == "PRI" || c.key == "UNI");
        out.push_str(&format!(
            "    {} {}--{} {} : \"{}\"\n",
            entity(parent),
            if optional { "|o" } else { "||" },
            if unique { "o|" } else { "o{" },
            entity(&f.table),
            f.name.replace('"', "'"),
        ));
    }
    out
}

fn plain(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Entity names may be quoted, so any table name survives.
fn entity(s: &str) -> String {
    if plain(s) { s.to_string() } else { format!("\"{}\"", s.replace('"', "'")) }
}

/// Attribute names and types cannot be quoted: other characters become `_`.
// ponytail: lossy for exotic column names; Mermaid has no escape for them.
fn word(s: &str) -> String {
    let w: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if plain(&w) { w } else { format!("_{w}") }
}

/// The save dialog picks the path; only `.mmd` files are written.
pub fn save(path: &str, text: &str) -> AppResult<()> {
    if Path::new(path).extension().is_none_or(|e| !e.eq_ignore_ascii_case("mmd")) {
        return Err(AppError::new("IO", path.to_string()));
    }
    Ok(std::fs::write(path, text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(table: &str, name: &str, data_type: &str, key: &str, nullable: bool) -> Column {
        Column { table: table.into(), name: name.into(), data_type: data_type.into(), key: key.into(), nullable }
    }
    fn fk(name: &str, table: &str, columns: &[&str], parent: Option<&str>) -> Fk {
        Fk { name: name.into(), table: table.into(), columns: columns.iter().map(|s| s.to_string()).collect(), parent: parent.map(Into::into) }
    }

    #[test]
    fn renders_entities_keys_and_relations() {
        let columns = vec![
            col("customers", "id", "int", "PRI", false),
            col("customers", "e mail", "varchar", "UNI", true),
            col("orders", "id", "int", "PRI", false),
            col("orders", "customer_id", "int", "MUL", false),
            col("orders", "agent_id", "int", "MUL", true),
            col("order items", "order_id", "int", "PRI", false),
            col("profiles", "customer_id", "int", "PRI", false),
            col("logs", "id", "bigint", "PRI", false),
        ];
        let fks = vec![
            fk("fk_cust", "orders", &["customer_id"], Some("customers")),
            fk("fk_agent", "orders", &["agent_id"], Some("customers")),
            fk("fk_\"o\"", "order items", &["order_id"], Some("orders")),
            fk("fk_prof", "profiles", &["customer_id"], Some("customers")),
            fk("fk_ext", "logs", &["id"], None),
        ];
        let all: Vec<String> = ["customers", "orders", "order items", "profiles", "logs"].map(String::from).to_vec();
        assert_eq!(
            render(&columns, &fks, &all, false),
            r#"erDiagram
    customers {
        int id PK
        varchar e_mail UK
    }
    orders {
        int id PK
        int customer_id FK
        int agent_id FK
    }
    "order items" {
        int order_id PK, FK
    }
    profiles {
        int customer_id PK, FK
    }
    logs {
        bigint id PK, FK
    }
    customers ||--o{ orders : "fk_cust"
    customers |o--o{ orders : "fk_agent"
    orders ||--o| "order items" : "fk_'o'"
    customers ||--o| profiles : "fk_prof"
"#
        );

        // One table alone: no relation. With its neighbors: both FK directions, one hop only.
        let orders = vec!["orders".to_string()];
        let alone = render(&columns, &fks, &orders, false);
        assert!(alone.contains("    orders {") && !alone.contains("customers") && !alone.contains("--"));
        let near = render(&columns, &fks, &orders, true);
        assert!(near.contains("    customers {") && near.contains("\"order items\" {") && !near.contains("profiles"));
        assert_eq!(near.matches("--").count(), 3);
    }

    #[test]
    fn words_and_save_guard() {
        assert_eq!(word("1st"), "_1st");
        assert_eq!(word("prix (€)"), "prix____");
        assert_eq!(entity("a\"b"), "\"a'b\"");
        assert_eq!(save("/tmp/x.sql", "").unwrap_err().code, "IO");
    }

    /// OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn diagram_on_test_server() {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let pool = Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        );
        let mut conn = pool.get_conn().await.unwrap();
        conn.query_drop(
            "DROP DATABASE IF EXISTS otaria_er; CREATE DATABASE otaria_er; USE otaria_er;
             CREATE TABLE a (id INT PRIMARY KEY, x INT, y INT, UNIQUE (x, y));
             CREATE TABLE b (id INT PRIMARY KEY, ax INT, ay INT NOT NULL, CONSTRAINT fk_ab FOREIGN KEY (ax, ay) REFERENCES a (x, y));",
        )
        .await
        .unwrap();
        let (columns, fks) = load(&pool, "otaria_er").await.unwrap();
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].columns, ["ax", "ay"]);
        let text = render(&columns, &fks, &["a".into(), "b".into()], false);
        assert!(text.contains("        int ax FK\n") && text.ends_with("    a |o--o{ b : \"fk_ab\"\n"), "{text}");
        conn.query_drop("DROP DATABASE otaria_er").await.unwrap();
    }
}
