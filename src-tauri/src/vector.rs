//! MariaDB VECTOR (11.7+): vector columns of a table, their vector index, and k-nearest-neighbour
//! search with `VEC_DISTANCE_EUCLIDEAN` / `VEC_DISTANCE_COSINE`. MariaDB only (tab hidden on MySQL).

use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use serde::{Deserialize, Serialize};

use crate::browse::{self, ident, Rows};
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VectorIndex {
    pub name: String,
    /// None = server default (`mhnsw_default_m`).
    pub m: Option<u32>,
    /// euclidean | cosine (None = server default, `mhnsw_default_distance`).
    pub distance: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorColumn {
    pub name: String,
    pub dimensions: u32,
    pub index: Option<VectorIndex>,
}

/// Vector indexes from `SHOW CREATE TABLE`: information_schema does not show M and DISTANCE.
/// Line shape (12.3): VECTOR KEY `ve` (`e`) `M`='8' `DISTANCE`='cosine'
pub fn vector_indexes(ddl: &str) -> Vec<(String, VectorIndex)> {
    ddl.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("VECTOR KEY ")?;
            let names = browse::quoted_names(rest);
            let (name, column) = (names.first()?, names.get(1)?);
            // Option names keep the case they were typed in (`M` or `m`): compare in lower case.
            let lower = rest.to_ascii_lowercase();
            let opt = |key: &str| {
                let at = lower.find(&format!("`{key}`='"))? + key.len() + 4;
                lower[at..].split('\'').next().map(str::to_string)
            };
            let index = VectorIndex { name: name.to_string(), m: opt("m").and_then(|m| m.parse().ok()), distance: opt("distance") };
            Some((column.to_string(), index))
        })
        .collect()
}

pub async fn columns(pool: &Pool, schema: &str, table: &str) -> AppResult<Vec<VectorColumn>> {
    let mut conn = pool.get_conn().await?;
    let cols: Vec<(String, String)> = conn
        .exec(
            "SELECT COLUMN_NAME, COLUMN_TYPE FROM information_schema.COLUMNS
              WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND DATA_TYPE = 'vector' ORDER BY ORDINAL_POSITION",
            (schema, table),
        )
        .await?;
    let indexes = vector_indexes(&browse::ddl_on(&mut conn, schema, table, "table").await?);
    Ok(cols
        .into_iter()
        .map(|(name, ty)| VectorColumn {
            dimensions: ty.trim_start_matches("vector(").trim_end_matches(')').parse().unwrap_or(0),
            index: indexes.iter().find(|(c, _)| *c == name).map(|(_, i)| i.clone()),
            name,
        })
        .collect())
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Distance {
    Euclidean,
    Cosine,
}

/// What to compare against: a vector typed as text (`[0.1, 0.2, …]`), or the vector of the row
/// whose single-column primary key is the given value.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum Probe {
    Text(String),
    Row(String),
}

/// The probe vector as an expression with one parameter (`?`).
fn probe_sql(schema: &str, table: &str, column: &str, key: Option<&str>) -> String {
    match key {
        Some(key) => format!("(SELECT {} FROM {}.{} WHERE {} = ?)", ident(column), ident(schema), ident(table), ident(key)),
        None => "VEC_FromText(?)".into(),
    }
}

/// The search query, distance first (named `_distance`). The probe is the only parameter (`?`).
/// `ORDER BY distance LIMIT k` on the indexed column is the shape the vector index serves.
pub fn search_sql(schema: &str, table: &str, column: &str, distance: Distance, key: Option<&str>, k: u32) -> String {
    let target = format!("{}.{}", ident(schema), ident(table));
    let col = ident(column);
    let probe = probe_sql(schema, table, column, key);
    let f = match distance {
        Distance::Euclidean => "VEC_DISTANCE_EUCLIDEAN",
        Distance::Cosine => "VEC_DISTANCE_COSINE",
    };
    format!("SELECT {f}({col}, {probe}) AS `_distance`, t.* FROM {target} t ORDER BY `_distance` LIMIT {k}")
}

pub async fn search(pool: &Pool, schema: &str, table: &str, column: &str, distance: Distance, probe: Probe, k: u32) -> AppResult<Rows> {
    if !(1..=1000).contains(&k) {
        return Err(AppError::new("INVALID_LIMIT", k.to_string()));
    }
    let mut conn = pool.get_conn().await?;
    let ty: Option<String> = conn
        .exec_first(
            "SELECT COLUMN_TYPE FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND COLUMN_NAME = ? AND DATA_TYPE = 'vector'",
            (schema, table, column),
        )
        .await?;
    let dims: u64 = ty.and_then(|t| t.trim_start_matches("vector(").trim_end_matches(')').parse().ok()).ok_or_else(|| AppError::new("NOT_FOUND", column.to_string()))?;
    let (key, value, row) = match probe {
        Probe::Text(v) => (None, v, false),
        Probe::Row(v) => {
            let pk: Vec<String> = conn
                .exec(
                    "SELECT COLUMN_NAME FROM information_schema.STATISTICS
                      WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND INDEX_NAME = 'PRIMARY'",
                    (schema, table),
                )
                .await?;
            match pk.as_slice() {
                [one] => (Some(one.clone()), v, true),
                _ => return Err(AppError::new("NO_SINGLE_KEY", table.to_string())),
            }
        }
    };
    // A bad probe gives NULL distances, not an error: check it first (4 bytes per dimension).
    let probe = probe_sql(schema, table, column, key.as_deref());
    let len: Option<Option<u64>> = conn.exec_first(format!("SELECT LENGTH({probe})"), (value.clone(),)).await?;
    match len.flatten() {
        None if row => return Err(AppError::new("NOT_FOUND", value)),
        None => return Err(AppError::new("INVALID_VECTOR", value)),
        Some(n) if n != dims * 4 => return Err(AppError::new("VECTOR_DIMENSIONS", format!("{} ≠ {dims}", n / 4))),
        _ => {}
    }
    let sql = search_sql(schema, table, column, distance, key.as_deref(), k);
    let mut result = conn.exec_iter(sql, (value,)).await?;
    let columns = result.columns_ref().iter().map(|c| c.name_str().into_owned()).collect();
    let rows = result.map(|row: mysql_async::Row| row.unwrap().into_iter().map(browse::cell).collect()).await?;
    drop(result);
    Ok(Rows { columns, rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_vector_indexes() {
        let ddl = "CREATE TABLE `v` (\n  `id` int(11) NOT NULL,\n  `e` vector(3) NOT NULL,\n  `f` vector(2) NOT NULL,\n  PRIMARY KEY (`id`),\n  VECTOR KEY `ve` (`e`) `M`='8' `DISTANCE`='COSINE',\n  VECTOR KEY `vf` (`f`)\n) ENGINE=InnoDB";
        assert_eq!(
            vector_indexes(ddl),
            [
                ("e".into(), VectorIndex { name: "ve".into(), m: Some(8), distance: Some("cosine".into()) }),
                ("f".into(), VectorIndex { name: "vf".into(), m: None, distance: None }),
            ]
        );
    }

    #[test]
    fn builds_search_queries() {
        assert_eq!(
            search_sql("s", "t`x", "e", Distance::Cosine, None, 5),
            "SELECT VEC_DISTANCE_COSINE(`e`, VEC_FromText(?)) AS `_distance`, t.* FROM `s`.`t``x` t ORDER BY `_distance` LIMIT 5"
        );
        assert_eq!(
            search_sql("s", "t", "e", Distance::Euclidean, Some("id"), 3),
            "SELECT VEC_DISTANCE_EUCLIDEAN(`e`, (SELECT `e` FROM `s`.`t` WHERE `id` = ?)) AS `_distance`, t.* FROM `s`.`t` t ORDER BY `_distance` LIMIT 3"
        );
    }

    /// OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn vector_on_test_server() {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let pool = Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        );
        pool.get_conn()
            .await
            .unwrap()
            .query_drop(
                "DROP DATABASE IF EXISTS otaria_vec; CREATE DATABASE otaria_vec; USE otaria_vec;
                 CREATE TABLE v (id INT PRIMARY KEY, e VECTOR(3) NOT NULL, VECTOR INDEX ve (e) M=8 DISTANCE=cosine);
                 INSERT INTO v VALUES (1, VEC_FromText('[1,0,0]')), (2, VEC_FromText('[0,1,0]')), (3, VEC_FromText('[0.9,0.1,0]'));",
            )
            .await
            .unwrap();
        let cols = columns(&pool, "otaria_vec", "v").await.unwrap();
        assert_eq!((cols[0].name.as_str(), cols[0].dimensions), ("e", 3));
        assert_eq!(cols[0].index.as_ref().unwrap().distance.as_deref(), Some("cosine"));

        let ids = |r: &Rows| r.rows.iter().map(|row| row[1].clone().unwrap()).collect::<Vec<_>>();
        let r = search(&pool, "otaria_vec", "v", "e", Distance::Cosine, Probe::Text("[1,0,0]".into()), 2).await.unwrap();
        assert_eq!(r.columns, ["_distance", "id", "e"]);
        assert_eq!(ids(&r), ["1", "3"]);
        let r = search(&pool, "otaria_vec", "v", "e", Distance::Euclidean, Probe::Row("2".into()), 3).await.unwrap();
        assert_eq!(ids(&r), ["2", "3", "1"]);
        let err = |p: Probe| async { search(&pool, "otaria_vec", "v", "e", Distance::Cosine, p, 2).await.unwrap_err() };
        assert_eq!(err(Probe::Text("nope".into())).await.code, "INVALID_VECTOR");
        assert_eq!(err(Probe::Row("9".into())).await.code, "NOT_FOUND");
        let e = err(Probe::Text("[1,2]".into())).await;
        assert_eq!((e.code, e.message.as_str()), ("VECTOR_DIMENSIONS", "2 ≠ 3"));

        pool.get_conn().await.unwrap().query_drop("DROP DATABASE otaria_vec").await.unwrap();
    }
}
