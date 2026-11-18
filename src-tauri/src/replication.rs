//! Replication and Galera (MariaDB only), read-only: replication channels from `SHOW ALL SLAVES STATUS`
//! (multi-source, one row per connection name), Galera cluster from the `wsrep_*` status and
//! `information_schema.WSREP_MEMBERSHIP`.
// ponytail: no START / STOP SLAVE, no CHANGE MASTER here: monitoring only; the SQL editor does the rest.

use std::collections::HashMap;

use mysql_async::prelude::Queryable;
use mysql_async::{Pool, Row};
use serde::Serialize;

use crate::error::AppResult;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    /// Empty for the default connection.
    pub name: String,
    pub master_host: String,
    pub master_port: u32,
    /// Yes | No | Connecting | Preparing
    pub io_running: String,
    pub sql_running: String,
    /// NULL when the SQL thread is stopped or not connected.
    pub seconds_behind: Option<u64>,
    /// No | Slave_Pos | Current_Pos
    pub using_gtid: String,
    pub gtid_io_pos: String,
    pub master_log_file: String,
    pub read_pos: u64,
    pub exec_pos: u64,
    pub io_state: String,
    pub sql_state: String,
    pub last_io_error: String,
    pub last_sql_error: String,
}

/// A row read by column name: the column set of SHOW SLAVE STATUS grows with each version.
fn by_name(row: Row) -> HashMap<String, String> {
    let names: Vec<String> = row.columns_ref().iter().map(|c| c.name_str().into_owned()).collect();
    names.into_iter().zip(row.unwrap().into_iter().map(crate::browse::cell)).filter_map(|(n, v)| Some((n, v?))).collect()
}

fn channel(m: &HashMap<String, String>) -> Channel {
    let s = |k: &str| m.get(k).cloned().unwrap_or_default();
    let n = |k: &str| m.get(k).and_then(|v| v.parse().ok());
    Channel {
        name: s("Connection_name"),
        master_host: s("Master_Host"),
        master_port: n("Master_Port").unwrap_or(0) as u32,
        io_running: s("Slave_IO_Running"),
        sql_running: s("Slave_SQL_Running"),
        seconds_behind: n("Seconds_Behind_Master"),
        using_gtid: s("Using_Gtid"),
        gtid_io_pos: s("Gtid_IO_Pos"),
        master_log_file: s("Master_Log_File"),
        read_pos: n("Read_Master_Log_Pos").unwrap_or(0),
        exec_pos: n("Exec_Master_Log_Pos").unwrap_or(0),
        io_state: s("Slave_IO_State"),
        sql_state: s("Slave_SQL_Running_State"),
        last_io_error: s("Last_IO_Error"),
        last_sql_error: s("Last_SQL_Error"),
    }
}

#[derive(Debug, Serialize)]
pub struct Node {
    pub index: u32,
    pub name: String,
    pub address: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Galera {
    pub cluster_name: String,
    pub node_name: String,
    /// The `wsrep_*` status values, prefix removed (cluster_size, cluster_status, local_state_comment,
    /// ready, connected, flow_control_paused, flow_control_sent, local_recv_queue_avg…).
    pub status: HashMap<String, String>,
    /// Empty without the SUPER privilege (the view then returns no rows) or the wsrep_info plugin.
    pub nodes: Vec<Node>,
}

#[derive(Debug, Serialize)]
pub struct State {
    pub channels: Vec<Channel>,
    /// None when the server is not a Galera node (`wsrep_on` = OFF).
    pub galera: Option<Galera>,
}

pub async fn state(pool: &Pool) -> AppResult<State> {
    let mut conn = pool.get_conn().await?;
    let rows: Vec<Row> = conn.query("SHOW ALL SLAVES STATUS").await?;
    let channels = rows.into_iter().map(|r| channel(&by_name(r))).collect();

    let (on, cluster_name, node_name): (bool, String, String) =
        conn.query_first("SELECT @@wsrep_on, @@wsrep_cluster_name, @@wsrep_node_name").await?.unwrap_or_default();
    let galera = if on {
        let status: Vec<(String, String)> = conn.query("SHOW GLOBAL STATUS LIKE 'wsrep\\_%'").await?;
        let status = status.into_iter().map(|(k, v)| (k.trim_start_matches("wsrep_").to_string(), v)).collect();
        let nodes = conn
            .query_map("SELECT `INDEX`, NAME, ADDRESS FROM information_schema.WSREP_MEMBERSHIP ORDER BY `INDEX`", |(index, name, address)| Node { index, name, address })
            .await
            .unwrap_or_default();
        Some(Galera { cluster_name, node_name, status, nodes })
    } else {
        None
    };
    Ok(State { channels, galera })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_channel_columns() {
        let m: HashMap<String, String> = [
            ("Connection_name", "src1"),
            ("Master_Host", "db1"),
            ("Master_Port", "3306"),
            ("Slave_IO_Running", "Yes"),
            ("Slave_SQL_Running", "No"),
            ("Read_Master_Log_Pos", "644"),
            ("Last_SQL_Error", "boom"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let c = channel(&m);
        assert_eq!((c.name.as_str(), c.master_port, c.read_pos, c.seconds_behind), ("src1", 3306, 644, None));
        assert_eq!((c.io_running.as_str(), c.sql_running.as_str(), c.last_sql_error.as_str()), ("Yes", "No", "boom"));
    }

    fn pool(var: &str) -> Pool {
        let spec = std::env::var(var).unwrap_or_else(|_| panic!("set {var}"));
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        Pool::new(mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false))
    }

    /// A replica with a running channel `src1` and a broken default channel, and a Galera node:
    /// OTARIA_TEST_REPLICA_DB=… OTARIA_TEST_GALERA_DB=… cargo test replication -- --ignored
    #[tokio::test]
    #[ignore]
    async fn replication_on_test_servers() {
        let st = state(&pool("OTARIA_TEST_REPLICA_DB")).await.unwrap();
        let src1 = st.channels.iter().find(|c| c.name == "src1").expect("src1");
        assert_eq!((src1.io_running.as_str(), src1.sql_running.as_str(), src1.seconds_behind), ("Yes", "Yes", Some(0)));
        assert_eq!(src1.using_gtid, "Slave_Pos");
        assert!(st.channels.iter().any(|c| c.name.is_empty() && c.io_running == "No"));
        assert!(st.galera.is_none());

        let g = state(&pool("OTARIA_TEST_GALERA_DB")).await.unwrap().galera.expect("galera");
        assert_eq!(g.cluster_name, "otaria");
        assert_eq!(g.status["cluster_status"], "Primary");
        assert_eq!(g.status["cluster_size"], g.nodes.len().to_string());
        assert!(g.nodes.iter().any(|n| n.name == g.node_name));
    }
}
