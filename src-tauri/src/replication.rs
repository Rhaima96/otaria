//! Replication and Galera (MariaDB only), read-only: replication channels from `SHOW ALL SLAVES STATUS`
//! (multi-source, one row per connection name), Galera cluster from the `wsrep_*` status and
//! `information_schema.WSREP_MEMBERSHIP`. Actions on a channel (start, stop, skip a failing event)
//! go through a preview of their SQL, like every other change, and so does CHANGE MASTER TO.

use std::collections::HashMap;

use mysql_async::prelude::Queryable;
use mysql_async::{Pool, Row};
use serde::{Deserialize, Serialize};

use crate::designer::literal;
use crate::error::{AppError, AppResult};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    /// Empty for the default connection.
    pub name: String,
    pub master_host: String,
    pub master_port: u32,
    pub master_user: String,
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
        master_user: s("Master_User"),
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

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Action {
    Start,
    Stop,
    /// Skip the next `count` events (transactions with GTID) of the SQL thread, then restart it:
    /// for a replica stopped on an error.
    Skip { count: u32 },
    /// CHANGE MASTER TO. `password` empty = unchanged; `gtid`: slave_pos | current_pos | no, and with
    /// `no`, `file` / `pos` (empty file = position left to the server).
    Change { host: String, port: u16, user: String, password: String, gtid: String, file: String, pos: u64 },
}

/// Statements for `action` on channel `name` ("" = the default connection), run in order on one
/// connection: `default_master_connection` is a session variable. `mask`: the password is replaced by
/// `<hidden>` (preview); the statement then does not parse, so it cannot be run by mistake.
pub fn statements(name: &str, action: &Action, mask: bool) -> AppResult<Vec<String>> {
    let ch = if name.is_empty() { String::new() } else { format!(" {}", literal(name)) };
    Ok(match action {
        Action::Start => vec![format!("START SLAVE{ch}")],
        Action::Stop => vec![format!("STOP SLAVE{ch}")],
        Action::Skip { count } => {
            if !(1..=1000).contains(count) {
                return Err(AppError::new("INVALID_NUMBER", count.to_string()));
            }
            vec![
                format!("STOP SLAVE{ch} SQL_THREAD"),
                format!("SET @@default_master_connection = {}", literal(name)),
                format!("SET GLOBAL sql_slave_skip_counter = {count}"),
                format!("START SLAVE{ch} SQL_THREAD"),
            ]
        }
        Action::Change { host, port, user, password, gtid, file, pos } => {
            if host.trim().is_empty() {
                return Err(AppError::new("REQUIRED", host.clone()));
            }
            if *port == 0 {
                return Err(AppError::new("INVALID_NUMBER", port.to_string()));
            }
            let mut opts = vec![format!("MASTER_HOST = {}", literal(host.trim())), format!("MASTER_PORT = {port}"), format!("MASTER_USER = {}", literal(user))];
            if !password.is_empty() {
                opts.push(format!("MASTER_PASSWORD = {}", if mask { "<hidden>".to_string() } else { literal(password) }));
            }
            match gtid.as_str() {
                "slave_pos" | "current_pos" => {}
                "no" if file.trim().is_empty() => {}
                "no" if *pos >= 4 => opts.extend([format!("MASTER_LOG_FILE = {}", literal(file.trim())), format!("MASTER_LOG_POS = {pos}")]),
                "no" => return Err(AppError::new("INVALID_NUMBER", pos.to_string())),
                _ => return Err(AppError::new("INVALID_NAME", gtid.clone())),
            }
            opts.push(format!("MASTER_USE_GTID = {gtid}"));
            vec![format!("CHANGE MASTER{ch} TO {}", opts.join(", "))]
        }
    })
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

    #[test]
    fn channel_actions() {
        assert_eq!(statements("", &Action::Start, false).unwrap(), ["START SLAVE"]);
        assert_eq!(statements("src'1", &Action::Stop, false).unwrap(), ["STOP SLAVE 'src''1'"]);
        assert_eq!(
            statements("src1", &Action::Skip { count: 2 }, false).unwrap(),
            ["STOP SLAVE 'src1' SQL_THREAD", "SET @@default_master_connection = 'src1'", "SET GLOBAL sql_slave_skip_counter = 2", "START SLAVE 'src1' SQL_THREAD"]
        );
        assert_eq!(statements("", &Action::Skip { count: 1 }, false).unwrap()[1], "SET @@default_master_connection = ''");
        assert_eq!(statements("x", &Action::Skip { count: 0 }, false).unwrap_err().code, "INVALID_NUMBER");
    }

    #[test]
    fn change_master() {
        let c = |gtid: &str, file: &str, pos: u64, password: &str| Action::Change {
            host: " db'1 ".into(), port: 3307, user: "rep".into(), password: password.into(), gtid: gtid.into(), file: file.into(), pos,
        };
        assert_eq!(
            statements("src1", &c("slave_pos", "ignored.000001", 0, "p'w"), false).unwrap(),
            ["CHANGE MASTER 'src1' TO MASTER_HOST = 'db''1', MASTER_PORT = 3307, MASTER_USER = 'rep', MASTER_PASSWORD = 'p''w', MASTER_USE_GTID = slave_pos"]
        );
        let shown = statements("src1", &c("slave_pos", "", 0, "secret"), true).unwrap().join("");
        assert!(shown.contains("MASTER_PASSWORD = <hidden>") && !shown.contains("secret"), "{shown}");
        assert_eq!(
            statements("", &c("no", "bin.000002", 342, ""), false).unwrap(),
            ["CHANGE MASTER TO MASTER_HOST = 'db''1', MASTER_PORT = 3307, MASTER_USER = 'rep', MASTER_LOG_FILE = 'bin.000002', MASTER_LOG_POS = 342, MASTER_USE_GTID = no"]
        );
        assert_eq!(statements("", &c("no", "bin.000002", 0, ""), false).unwrap_err().code, "INVALID_NUMBER");
        assert_eq!(statements("", &c("own_pos; DROP", "", 0, ""), false).unwrap_err().code, "INVALID_NAME");
        assert_eq!(statements("", &Action::Change { host: " ".into(), port: 1, user: String::new(), password: String::new(), gtid: "no".into(), file: String::new(), pos: 0 }, false).unwrap_err().code, "REQUIRED");
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

    /// Start, stop and skip on a channel the test sets up itself: a replica (OTARIA_TEST_REPLICA_ACTIONS_DB)
    /// of the binlog server (OTARIA_TEST_BINLOG_DB, seen from the replica as OTARIA_TEST_MASTER_HOST).
    #[tokio::test]
    #[ignore]
    async fn replication_actions_on_test_servers() {
        let master = pool("OTARIA_TEST_BINLOG_DB");
        let replica = pool("OTARIA_TEST_REPLICA_ACTIONS_DB");
        let host = std::env::var("OTARIA_TEST_MASTER_HOST").unwrap_or_else(|_| "otaria-test-binlog".into());
        let pw = std::env::var("OTARIA_TEST_BINLOG_DB").unwrap().splitn(4, ':').nth(3).unwrap().to_string();
        let mut m = master.get_conn().await.unwrap();
        let mut r = replica.get_conn().await.unwrap();
        m.query_drop("DROP DATABASE IF EXISTS ot_rep; CREATE DATABASE ot_rep; CREATE TABLE ot_rep.t (id INT PRIMARY KEY)").await.unwrap();
        let gtid: String = m.query_first("SELECT @@gtid_binlog_pos").await.unwrap().unwrap();
        r.query_drop(format!(
            "STOP ALL SLAVES; DROP DATABASE IF EXISTS ot_rep; CREATE DATABASE ot_rep; CREATE TABLE ot_rep.t (id INT PRIMARY KEY);
             SET GLOBAL gtid_slave_pos = '{gtid}'"
        ))
        .await
        .unwrap();
        let change = Action::Change { host, port: 3306, user: "root".into(), password: pw, gtid: "slave_pos".into(), file: String::new(), pos: 0 };
        crate::designer::run(&replica, &statements("ot", &change, false).unwrap()).await.unwrap();
        let wait = |cond: fn(&Channel) -> bool| {
            let replica = replica.clone();
            async move {
                for _ in 0..50 {
                    let st = state(&replica).await.unwrap();
                    if let Some(c) = st.channels.into_iter().find(|c| c.name == "ot").filter(cond) {
                        return c;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                panic!("channel never reached the expected state");
            }
        };
        crate::designer::run(&replica, &statements("ot", &Action::Start, false).unwrap()).await.unwrap();
        wait(|c| c.io_running == "Yes" && c.sql_running == "Yes").await;

        // A row the replica already has: the SQL thread stops on 1062, skipping one transaction resumes it.
        r.query_drop("INSERT INTO ot_rep.t VALUES (1)").await.unwrap();
        m.query_drop("INSERT INTO ot_rep.t VALUES (1); INSERT INTO ot_rep.t VALUES (2)").await.unwrap();
        let broken = wait(|c| c.sql_running == "No").await;
        assert!(broken.last_sql_error.contains("Duplicate"), "{broken:?}");
        crate::designer::run(&replica, &statements("ot", &Action::Skip { count: 1 }, false).unwrap()).await.unwrap();
        wait(|c| c.sql_running == "Yes").await;
        for _ in 0..50 {
            let n: Option<u64> = r.query_first("SELECT COUNT(*) FROM ot_rep.t").await.unwrap();
            if n == Some(2) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let n: Option<u64> = r.query_first("SELECT COUNT(*) FROM ot_rep.t").await.unwrap();
        assert_eq!(n, Some(2), "the transaction after the skipped one was applied");

        crate::designer::run(&replica, &statements("ot", &Action::Stop, false).unwrap()).await.unwrap();
        wait(|c| c.io_running == "No" && c.sql_running == "No").await;
        r.query_drop("RESET SLAVE 'ot' ALL; DROP DATABASE ot_rep").await.unwrap();
        m.query_drop("DROP DATABASE ot_rep").await.unwrap();
    }
}
