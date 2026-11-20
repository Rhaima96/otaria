//! Replication and Galera: replication channels from `SHOW ALL SLAVES STATUS` (MariaDB, one row per
//! connection name) or `SHOW REPLICA STATUS` (MySQL 8.0.22+, one row per channel); Galera cluster (MariaDB
//! only) from the `wsrep_*` status and `information_schema.WSREP_MEMBERSHIP`. Actions on a channel (start,
//! stop, skip a failing event) go through a preview of their SQL, like every other change, and so does
//! CHANGE MASTER TO / CHANGE REPLICATION SOURCE TO.

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
    /// MariaDB: No | Slave_Pos | Current_Pos. MySQL: No | Auto (SOURCE_AUTO_POSITION = 1).
    pub using_gtid: String,
    /// MariaDB Gtid_IO_Pos, MySQL Retrieved_Gtid_Set.
    pub gtid_io_pos: String,
    pub master_log_file: String,
    pub read_pos: u64,
    pub exec_pos: u64,
    pub io_state: String,
    pub sql_state: String,
    pub last_io_error: String,
    pub last_sql_error: String,
    /// TLS to the source (Master_SSL_Allowed / Source_SSL_Allowed = Yes).
    pub ssl: bool,
}

/// A row read by column name: the column set of SHOW SLAVE STATUS grows with each version.
fn by_name(row: Row) -> HashMap<String, String> {
    let names: Vec<String> = row.columns_ref().iter().map(|c| c.name_str().into_owned()).collect();
    names.into_iter().zip(row.unwrap().into_iter().map(crate::browse::cell)).filter_map(|(n, v)| Some((n, v?))).collect()
}

/// Columns are named by the MariaDB words; MySQL's SHOW REPLICA STATUS says Source / Replica instead
/// (Master_Host → Source_Host, Slave_IO_Running → Replica_IO_Running…), and names the channel Channel_Name.
fn channel(m: &HashMap<String, String>) -> Channel {
    let get = |k: &str| m.get(k).or_else(|| m.get(&k.replace("Master", "Source").replace("Slave", "Replica")));
    let s = |k: &str| get(k).cloned().unwrap_or_default();
    let n = |k: &str| get(k).and_then(|v| v.parse().ok());
    let mysql = m.contains_key("Channel_Name");
    Channel {
        name: if mysql { s("Channel_Name") } else { s("Connection_name") },
        master_host: s("Master_Host"),
        master_port: n("Master_Port").unwrap_or(0) as u32,
        master_user: s("Master_User"),
        io_running: s("Slave_IO_Running"),
        sql_running: s("Slave_SQL_Running"),
        seconds_behind: n("Seconds_Behind_Master"),
        using_gtid: if mysql { (if s("Auto_Position") == "1" { "Auto" } else { "No" }).to_string() } else { s("Using_Gtid") },
        // MySQL writes a newline after each comma of a long GTID set.
        gtid_io_pos: if mysql { s("Retrieved_Gtid_Set").replace('\n', "") } else { s("Gtid_IO_Pos") },
        master_log_file: s("Master_Log_File"),
        read_pos: n("Read_Master_Log_Pos").unwrap_or(0),
        exec_pos: n("Exec_Master_Log_Pos").unwrap_or(0),
        io_state: s("Slave_IO_State"),
        sql_state: s("Slave_SQL_Running_State"),
        last_io_error: s("Last_IO_Error"),
        last_sql_error: s("Last_SQL_Error"),
        ssl: s("Master_SSL_Allowed") == "Yes",
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
    /// None when the server is not a Galera node (`wsrep_on` = OFF), and always on MySQL.
    pub galera: Option<Galera>,
}

/// Which replication words the server speaks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dialect {
    MariaDb,
    /// MySQL up to 8.0.25: SLAVE / MASTER (SHOW SLAVE STATUS, CHANGE MASTER TO, sql_slave_skip_counter).
    MySqlOld,
    /// MySQL 8.0.26+: REPLICA / SOURCE (the old words are gone in 8.4).
    MySql,
}

impl Dialect {
    pub fn of(mariadb: bool, version: &str) -> Self {
        match (mariadb, crate::db::version_at_least(version, (8, 0, 26))) {
            (true, _) => Dialect::MariaDb,
            (false, true) => Dialect::MySql,
            (false, false) => Dialect::MySqlOld,
        }
    }
}

pub async fn state(pool: &Pool, dialect: Dialect) -> AppResult<State> {
    let mut conn = pool.get_conn().await?;
    let sql = match dialect {
        Dialect::MariaDb => "SHOW ALL SLAVES STATUS",
        Dialect::MySqlOld => "SHOW SLAVE STATUS",
        Dialect::MySql => "SHOW REPLICA STATUS",
    };
    let rows: Vec<Row> = conn.query(sql).await?;
    let mut channels: Vec<Channel> = rows.into_iter().map(|r| channel(&by_name(r))).collect();
    if dialect != Dialect::MariaDb {
        // The multithreaded applier only says "Worker n failed…": the cause is in the worker's own error.
        let workers: Vec<(String, String)> = conn
            .query("SELECT CHANNEL_NAME, LAST_ERROR_MESSAGE FROM performance_schema.replication_applier_status_by_worker WHERE LAST_ERROR_NUMBER <> 0")
            .await
            .unwrap_or_default();
        for c in channels.iter_mut().filter(|c| !c.last_sql_error.is_empty()) {
            if let Some((_, cause)) = workers.iter().find(|w| w.0 == c.name).filter(|w| !c.last_sql_error.contains(&w.1)) {
                c.last_sql_error = format!("{cause} — {}", c.last_sql_error);
            }
        }
        return Ok(State { channels, galera: None });
    }

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
    /// CHANGE MASTER TO (MySQL 8.0.26+: CHANGE REPLICATION SOURCE TO). `password` empty = unchanged; `gtid`:
    /// slave_pos | current_pos | no on MariaDB, auto | no on MySQL (SOURCE_AUTO_POSITION), and with `no`,
    /// `file` / `pos` (empty file = position left to the server). `ssl`: TLS to the source (MASTER_SSL).
    Change {
        host: String,
        port: u16,
        user: String,
        password: String,
        gtid: String,
        file: String,
        pos: u64,
        #[serde(default)]
        ssl: bool,
    },
}

/// Statements for `action` on channel `name` ("" = the default connection), run in order on one
/// connection: `default_master_connection` is a session variable. `mask`: the password is replaced by
/// `<hidden>` (preview); the statement then does not parse, so it cannot be run by mistake.
pub fn statements(name: &str, action: &Action, mask: bool, dialect: Dialect) -> AppResult<Vec<String>> {
    if dialect != Dialect::MariaDb {
        return mysql_statements(name, action, mask, dialect == Dialect::MySql);
    }
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
        Action::Change { host, port, user, password, gtid, file, pos, ssl } => {
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
            opts.push(format!("MASTER_SSL = {}", u8::from(*ssl)));
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

/// MySQL: `FOR CHANNEL` at the end. `new_words` (8.0.26+): REPLICA / SOURCE, else SLAVE / MASTER. The skip
/// counter is global and used by the next START … FOR CHANNEL; MySQL refuses it with auto-positioning (GTID).
fn mysql_statements(name: &str, action: &Action, mask: bool, new_words: bool) -> AppResult<Vec<String>> {
    let ch = if name.is_empty() { String::new() } else { format!(" FOR CHANNEL {}", literal(name)) };
    let (replica, src, skip_var, change) = if new_words {
        ("REPLICA", "SOURCE", "sql_replica_skip_counter", "CHANGE REPLICATION SOURCE TO")
    } else {
        ("SLAVE", "MASTER", "sql_slave_skip_counter", "CHANGE MASTER TO")
    };
    Ok(match action {
        Action::Start => vec![format!("START {replica}{ch}")],
        Action::Stop => vec![format!("STOP {replica}{ch}")],
        Action::Skip { count } => {
            if !(1..=1000).contains(count) {
                return Err(AppError::new("INVALID_NUMBER", count.to_string()));
            }
            vec![
                format!("STOP {replica} SQL_THREAD{ch}"),
                format!("SET GLOBAL {skip_var} = {count}"),
                format!("START {replica} SQL_THREAD{ch}"),
            ]
        }
        Action::Change { host, port, user, password, gtid, file, pos, ssl } => {
            if host.trim().is_empty() {
                return Err(AppError::new("REQUIRED", host.clone()));
            }
            if *port == 0 {
                return Err(AppError::new("INVALID_NUMBER", port.to_string()));
            }
            let mut opts = vec![format!("{src}_HOST = {}", literal(host.trim())), format!("{src}_PORT = {port}"), format!("{src}_USER = {}", literal(user))];
            if !password.is_empty() {
                opts.push(format!("{src}_PASSWORD = {}", if mask { "<hidden>".to_string() } else { literal(password) }));
            }
            opts.push(format!("{src}_SSL = {}", u8::from(*ssl)));
            match gtid.as_str() {
                "auto" => opts.push(format!("{src}_AUTO_POSITION = 1")),
                "no" if file.trim().is_empty() => opts.push(format!("{src}_AUTO_POSITION = 0")),
                "no" if *pos >= 4 => opts.extend([
                    format!("{src}_AUTO_POSITION = 0"),
                    format!("{src}_LOG_FILE = {}", literal(file.trim())),
                    format!("{src}_LOG_POS = {pos}"),
                ]),
                "no" => return Err(AppError::new("INVALID_NUMBER", pos.to_string())),
                _ => return Err(AppError::new("INVALID_NAME", gtid.clone())),
            }
            vec![format!("{change} {}{ch}", opts.join(", "))]
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
        assert_eq!(statements("", &Action::Start, false, Dialect::MariaDb).unwrap(), ["START SLAVE"]);
        assert_eq!(statements("src'1", &Action::Stop, false, Dialect::MariaDb).unwrap(), ["STOP SLAVE 'src''1'"]);
        assert_eq!(
            statements("src1", &Action::Skip { count: 2 }, false, Dialect::MariaDb).unwrap(),
            ["STOP SLAVE 'src1' SQL_THREAD", "SET @@default_master_connection = 'src1'", "SET GLOBAL sql_slave_skip_counter = 2", "START SLAVE 'src1' SQL_THREAD"]
        );
        assert_eq!(statements("", &Action::Skip { count: 1 }, false, Dialect::MariaDb).unwrap()[1], "SET @@default_master_connection = ''");
        assert_eq!(statements("x", &Action::Skip { count: 0 }, false, Dialect::MariaDb).unwrap_err().code, "INVALID_NUMBER");
    }

    #[test]
    fn change_master() {
        let c = |gtid: &str, file: &str, pos: u64, password: &str| Action::Change {
            host: " db'1 ".into(), port: 3307, user: "rep".into(), password: password.into(), gtid: gtid.into(), file: file.into(), pos, ssl: true,
        };
        assert_eq!(
            statements("src1", &c("slave_pos", "ignored.000001", 0, "p'w"), false, Dialect::MariaDb).unwrap(),
            ["CHANGE MASTER 'src1' TO MASTER_HOST = 'db''1', MASTER_PORT = 3307, MASTER_USER = 'rep', MASTER_PASSWORD = 'p''w', MASTER_SSL = 1, MASTER_USE_GTID = slave_pos"]
        );
        let shown = statements("src1", &c("slave_pos", "", 0, "secret"), true, Dialect::MariaDb).unwrap().join("");
        assert!(shown.contains("MASTER_PASSWORD = <hidden>") && !shown.contains("secret"), "{shown}");
        assert_eq!(
            statements("", &c("no", "bin.000002", 342, ""), false, Dialect::MariaDb).unwrap(),
            ["CHANGE MASTER TO MASTER_HOST = 'db''1', MASTER_PORT = 3307, MASTER_USER = 'rep', MASTER_SSL = 1, MASTER_LOG_FILE = 'bin.000002', MASTER_LOG_POS = 342, MASTER_USE_GTID = no"]
        );
        assert_eq!(statements("", &c("no", "bin.000002", 0, ""), false, Dialect::MariaDb).unwrap_err().code, "INVALID_NUMBER");
        assert_eq!(statements("", &c("own_pos; DROP", "", 0, ""), false, Dialect::MariaDb).unwrap_err().code, "INVALID_NAME");
        assert_eq!(statements("", &Action::Change { host: " ".into(), port: 1, user: String::new(), password: String::new(), gtid: "no".into(), file: String::new(), pos: 0, ssl: false }, false, Dialect::MariaDb).unwrap_err().code, "REQUIRED");
    }

    #[test]
    fn reads_mysql_columns() {
        let m: HashMap<String, String> = [
            ("Channel_Name", "src1"),
            ("Source_Host", "db1"),
            ("Source_Port", "3307"),
            ("Replica_IO_Running", "Yes"),
            ("Replica_SQL_Running", "No"),
            ("Seconds_Behind_Source", "5"),
            ("Exec_Source_Log_Pos", "601"),
            ("Auto_Position", "1"),
            ("Retrieved_Gtid_Set", "a:1-5,\nb:1"),
            ("Last_SQL_Error", "boom"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let c = channel(&m);
        assert_eq!((c.name.as_str(), c.master_host.as_str(), c.master_port, c.seconds_behind, c.exec_pos), ("src1", "db1", 3307, Some(5), 601));
        assert_eq!((c.io_running.as_str(), c.sql_running.as_str(), c.last_sql_error.as_str()), ("Yes", "No", "boom"));
        assert_eq!((c.using_gtid.as_str(), c.gtid_io_pos.as_str()), ("Auto", "a:1-5,b:1"));
    }

    #[test]
    fn mysql_statements_use_replica_words() {
        assert_eq!(statements("", &Action::Start, false, Dialect::MySql).unwrap(), ["START REPLICA"]);
        assert_eq!(statements("src'1", &Action::Stop, false, Dialect::MySql).unwrap(), ["STOP REPLICA FOR CHANNEL 'src''1'"]);
        assert_eq!(
            statements("src1", &Action::Skip { count: 2 }, false, Dialect::MySql).unwrap(),
            ["STOP REPLICA SQL_THREAD FOR CHANNEL 'src1'", "SET GLOBAL sql_replica_skip_counter = 2", "START REPLICA SQL_THREAD FOR CHANNEL 'src1'"]
        );
        let c = |gtid: &str, file: &str, pos: u64, password: &str| Action::Change {
            host: " db'1 ".into(), port: 3307, user: "rep".into(), password: password.into(), gtid: gtid.into(), file: file.into(), pos, ssl: true,
        };
        assert_eq!(
            statements("src1", &c("auto", "ignored", 0, "p'w"), false, Dialect::MySql).unwrap(),
            ["CHANGE REPLICATION SOURCE TO SOURCE_HOST = 'db''1', SOURCE_PORT = 3307, SOURCE_USER = 'rep', SOURCE_PASSWORD = 'p''w', SOURCE_SSL = 1, SOURCE_AUTO_POSITION = 1 FOR CHANNEL 'src1'"]
        );
        assert!(statements("", &c("auto", "", 0, "secret"), true, Dialect::MySql).unwrap()[0].contains("SOURCE_PASSWORD = <hidden>"));
        assert_eq!(
            statements("", &c("no", "binlog.000002", 342, ""), false, Dialect::MySql).unwrap(),
            ["CHANGE REPLICATION SOURCE TO SOURCE_HOST = 'db''1', SOURCE_PORT = 3307, SOURCE_USER = 'rep', SOURCE_SSL = 1, SOURCE_AUTO_POSITION = 0, SOURCE_LOG_FILE = 'binlog.000002', SOURCE_LOG_POS = 342"]
        );
        // MySQL before 8.0.26: SLAVE / MASTER words.
        assert_eq!(
            statements("src1", &Action::Skip { count: 1 }, false, Dialect::MySqlOld).unwrap(),
            ["STOP SLAVE SQL_THREAD FOR CHANNEL 'src1'", "SET GLOBAL sql_slave_skip_counter = 1", "START SLAVE SQL_THREAD FOR CHANNEL 'src1'"]
        );
        assert_eq!(
            statements("", &c("auto", "", 0, ""), false, Dialect::MySqlOld).unwrap(),
            ["CHANGE MASTER TO MASTER_HOST = 'db''1', MASTER_PORT = 3307, MASTER_USER = 'rep', MASTER_SSL = 1, MASTER_AUTO_POSITION = 1"]
        );
        assert_eq!((Dialect::of(false, "5.7.44-log"), Dialect::of(false, "8.0.25"), Dialect::of(false, "8.0.46"), Dialect::of(true, "12.3.3-MariaDB")), (Dialect::MySqlOld, Dialect::MySqlOld, Dialect::MySql, Dialect::MariaDb));
        // MariaDB's GTID modes do not exist on MySQL, and the other way round.
        assert_eq!(statements("", &c("slave_pos", "", 0, ""), false, Dialect::MySql).unwrap_err().code, "INVALID_NAME");
        assert_eq!(statements("", &c("auto", "", 0, ""), false, Dialect::MariaDb).unwrap_err().code, "INVALID_NAME");
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
        let st = state(&pool("OTARIA_TEST_REPLICA_DB"), Dialect::MariaDb).await.unwrap();
        let src1 = st.channels.iter().find(|c| c.name == "src1").expect("src1");
        assert_eq!((src1.io_running.as_str(), src1.sql_running.as_str(), src1.seconds_behind), ("Yes", "Yes", Some(0)));
        assert_eq!(src1.using_gtid, "Slave_Pos");
        assert!(st.channels.iter().any(|c| c.name.is_empty() && c.io_running == "No"));
        assert!(st.galera.is_none());

        let g = state(&pool("OTARIA_TEST_GALERA_DB"), Dialect::MariaDb).await.unwrap().galera.expect("galera");
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
        let change = Action::Change { host, port: 3306, user: "root".into(), password: pw, gtid: "slave_pos".into(), file: String::new(), pos: 0, ssl: true };
        crate::designer::run(&replica, &statements("ot", &change, false, Dialect::MariaDb).unwrap()).await.unwrap();
        let wait = |cond: fn(&Channel) -> bool| {
            let replica = replica.clone();
            async move {
                for _ in 0..50 {
                    let st = state(&replica, Dialect::MariaDb).await.unwrap();
                    if let Some(c) = st.channels.into_iter().find(|c| c.name == "ot").filter(cond) {
                        return c;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                panic!("channel never reached the expected state");
            }
        };
        crate::designer::run(&replica, &statements("ot", &Action::Start, false, Dialect::MariaDb).unwrap()).await.unwrap();
        wait(|c| c.io_running == "Yes" && c.sql_running == "Yes").await;

        // A row the replica already has: the SQL thread stops on 1062, skipping one transaction resumes it.
        r.query_drop("INSERT INTO ot_rep.t VALUES (1)").await.unwrap();
        m.query_drop("INSERT INTO ot_rep.t VALUES (1); INSERT INTO ot_rep.t VALUES (2)").await.unwrap();
        let broken = wait(|c| c.sql_running == "No").await;
        assert!(broken.last_sql_error.contains("Duplicate"), "{broken:?}");
        crate::designer::run(&replica, &statements("ot", &Action::Skip { count: 1 }, false, Dialect::MariaDb).unwrap()).await.unwrap();
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

        crate::designer::run(&replica, &statements("ot", &Action::Stop, false, Dialect::MariaDb).unwrap()).await.unwrap();
        wait(|c| c.io_running == "No" && c.sql_running == "No").await;
        r.query_drop("RESET SLAVE 'ot' ALL; DROP DATABASE ot_rep").await.unwrap();
        m.query_drop("DROP DATABASE ot_rep").await.unwrap();
    }

    /// MySQL 5.7 / 8.0 / 8.4: a replica (OTARIA_TEST_MYSQL_REPLICA) of OTARIA_TEST_MYSQL, which it reaches as
    /// OTARIA_TEST_MYSQL_SOURCE_HOST with the account repl / replpw (REPLICATION SLAVE). The test sets up its
    /// own channel `ot` with TLS, then starts, breaks, skips and stops it, in the words of the replica's version.
    #[tokio::test]
    #[ignore]
    async fn replication_on_mysql() {
        let source = pool("OTARIA_TEST_MYSQL");
        let replica = pool("OTARIA_TEST_MYSQL_REPLICA");
        let host = std::env::var("OTARIA_TEST_MYSQL_SOURCE_HOST").unwrap_or_else(|_| "otaria-test-mysql".into());
        let mut m = source.get_conn().await.unwrap();
        let mut r = replica.get_conn().await.unwrap();
        let version: String = r.query_first("SELECT VERSION()").await.unwrap().unwrap();
        let dialect = Dialect::of(false, &version);
        let (replica_word, status) = if dialect == Dialect::MySql { ("REPLICA", "SHOW BINARY LOG STATUS") } else { ("SLAVE", "SHOW MASTER STATUS") };
        let status = if crate::db::version_at_least(&version, (8, 2, 0)) { status } else { "SHOW MASTER STATUS" };
        m.query_drop("DROP DATABASE IF EXISTS ot_rep; CREATE DATABASE ot_rep; CREATE TABLE ot_rep.t (id INT PRIMARY KEY)").await.unwrap();
        let mut at: Row = m.query_first(status).await.unwrap().unwrap();
        let (file, pos): (String, u64) = (at.take(0).unwrap(), at.take(1).unwrap());
        r.query_drop(format!("STOP {replica_word} FOR CHANNEL 'ot'; RESET {replica_word} ALL FOR CHANNEL 'ot'")).await.ok();
        r.query_drop("DROP DATABASE IF EXISTS ot_rep; CREATE DATABASE ot_rep; CREATE TABLE ot_rep.t (id INT PRIMARY KEY)").await.unwrap();
        // ssl: true: a caching_sha2_password account (MySQL 8) is refused over a plain link.
        let change = Action::Change { host, port: 3306, user: "repl".into(), password: "replpw".into(), gtid: "no".into(), file, pos, ssl: true };
        crate::designer::run(&replica, &statements("ot", &change, false, dialect).unwrap()).await.unwrap();
        let wait = |cond: fn(&Channel) -> bool| {
            let replica = replica.clone();
            async move {
                for _ in 0..50 {
                    let st = state(&replica, dialect).await.unwrap();
                    assert!(st.galera.is_none());
                    if let Some(c) = st.channels.into_iter().find(|c| c.name == "ot").filter(cond) {
                        return c;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                panic!("channel never reached the expected state");
            }
        };
        crate::designer::run(&replica, &statements("ot", &Action::Start, false, dialect).unwrap()).await.unwrap();
        let running = wait(|c| c.io_running == "Yes" && c.sql_running == "Yes").await;
        assert_eq!((running.master_user.as_str(), running.using_gtid.as_str(), running.ssl), ("repl", "No", true));

        // A row the replica already has: the SQL thread stops on 1062, skipping one transaction resumes it.
        r.query_drop("INSERT INTO ot_rep.t VALUES (1)").await.unwrap();
        m.query_drop("INSERT INTO ot_rep.t VALUES (1); INSERT INTO ot_rep.t VALUES (2)").await.unwrap();
        let broken = wait(|c| c.sql_running == "No").await;
        assert!(broken.last_sql_error.contains("Duplicate"), "worker cause shown first: {broken:?}");
        crate::designer::run(&replica, &statements("ot", &Action::Skip { count: 1 }, false, dialect).unwrap()).await.unwrap();
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

        crate::designer::run(&replica, &statements("ot", &Action::Stop, false, dialect).unwrap()).await.unwrap();
        wait(|c| c.io_running == "No" && c.sql_running == "No").await;
        r.query_drop(format!("RESET {replica_word} ALL FOR CHANNEL 'ot'; DROP DATABASE ot_rep")).await.unwrap();
        m.query_drop("DROP DATABASE ot_rep").await.unwrap();
    }
}
