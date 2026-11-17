//! Server plugins (MariaDB only): list `information_schema.ALL_PLUGINS`, install the ones whose
//! library is already in `plugin_dir`. Installing an OS package (e.g. DuckDB) is out of reach from SQL.
// ponytail: no UNINSTALL here (can orphan tables or accounts); the SQL editor covers it until asked.

use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use serde::Serialize;

use crate::browse::ident;
use crate::error::{AppError, AppResult};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plugin {
    pub name: String,
    pub kind: String,
    /// ACTIVE | INACTIVE | DISABLED | DELETED | NOT INSTALLED
    pub status: String,
    /// None for plugins built into the server.
    pub library: Option<String>,
    pub maturity: String,
    pub description: String,
}

pub async fn list(pool: &Pool) -> AppResult<Vec<Plugin>> {
    Ok(pool
        .get_conn()
        .await?
        .query_map(
            "SELECT PLUGIN_NAME, PLUGIN_TYPE, PLUGIN_STATUS, PLUGIN_LIBRARY, PLUGIN_MATURITY, IFNULL(PLUGIN_DESCRIPTION, '')
             FROM information_schema.ALL_PLUGINS ORDER BY PLUGIN_TYPE, PLUGIN_NAME",
            |(name, kind, status, library, maturity, description)| Plugin { name, kind, status, library, maturity, description },
        )
        .await?)
}

/// SONAME is a string literal, not a parameter: only plain file names (`ha_connect.so`) get through.
pub fn install_sql(name: &str, library: &str) -> AppResult<String> {
    let ok = !library.is_empty() && library.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) && !library.starts_with('.');
    if !ok {
        return Err(AppError::new("PLUGIN_UNAVAILABLE", library.to_string()));
    }
    Ok(format!("INSTALL PLUGIN {} SONAME '{library}'", ident(name)))
}

/// Runs only for a plugin the server itself lists as NOT INSTALLED with that library.
pub async fn install(pool: &Pool, name: &str, library: &str) -> AppResult<()> {
    let sql = install_sql(name, library)?;
    let mut conn = pool.get_conn().await?;
    let found: Option<u8> = conn
        .exec_first(
            "SELECT 1 FROM information_schema.ALL_PLUGINS WHERE PLUGIN_NAME = ? AND PLUGIN_LIBRARY = ? AND PLUGIN_STATUS = 'NOT INSTALLED'",
            (name, library),
        )
        .await?;
    if found.is_none() {
        return Err(AppError::new("PLUGIN_UNAVAILABLE", name.to_string()));
    }
    conn.query_drop(sql).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_sql_quotes_name_and_rejects_paths() {
        assert_eq!(install_sql("CONNECT", "ha_connect.so").unwrap(), "INSTALL PLUGIN `CONNECT` SONAME 'ha_connect.so'");
        assert_eq!(install_sql("a`b", "x.so").unwrap(), "INSTALL PLUGIN `a``b` SONAME 'x.so'");
        for bad in ["", "../x.so", "/lib/x.so", "x.so'; DROP TABLE t; --", ".hidden"] {
            assert_eq!(install_sql("p", bad).unwrap_err().code, "PLUGIN_UNAVAILABLE", "{bad}");
        }
    }

    /// OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn plugins_on_test_server() {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let pool = Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        );
        let all = list(&pool).await.unwrap();
        assert!(all.iter().any(|x| x.name == "InnoDB" && x.status == "ACTIVE"));
        // Built-in plugins are never NOT INSTALLED, so the whitelist refuses them.
        assert_eq!(install(&pool, "InnoDB", "ha_innodb.so").await.unwrap_err().code, "PLUGIN_UNAVAILABLE");
        // DISKS: a harmless information_schema plugin shipped with the server.
        let mut conn = pool.get_conn().await.unwrap();
        conn.query_drop("UNINSTALL SONAME IF EXISTS 'disks.so'").await.unwrap();
        install(&pool, "DISKS", "disks.so").await.unwrap();
        let st: Option<String> = conn.query_first("SELECT PLUGIN_STATUS FROM information_schema.PLUGINS WHERE PLUGIN_NAME = 'DISKS'").await.unwrap();
        assert_eq!(st.as_deref(), Some("ACTIVE"));
        assert_eq!(install(&pool, "DISKS", "disks.so").await.unwrap_err().code, "PLUGIN_UNAVAILABLE");
        conn.query_drop("UNINSTALL SONAME 'disks.so'").await.unwrap();
    }
}
