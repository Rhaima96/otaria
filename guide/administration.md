# Server administration

The server tab gives access to the administration screens. Each opens in its own tab. Lists can refresh on their own every few seconds; set the interval or pause it with **Refresh**.

What you see depends on your privileges: for example, other users' processes need `PROCESS`, and accounts need access to the `mysql` database.

## Processes

Every connection to the server, with its user, host, database, command, time, state, statement and progress. **Hide idle connections** hides sleeping ones. Otaria's own connections are marked **Otaria**.

- **Execution plan** shows the plan of a running statement (MariaDB 10.9+, MySQL 5.7+). The server answers when the query reaches a checkpoint, which can take a few seconds.
- **Stop the query** stops the statement; the connection stays open.
- **Close the connection** closes it; its open transaction is rolled back.

## Variables

Global variables with their value, default, origin, type and range. **Changed from default only** narrows the list.

Click a value to set it (`SET GLOBAL`) or reset it to the default. `SET GLOBAL` applies to new connections and is lost at restart unless you also write it in the server's configuration file.

## Status

Queries per second, connections (running and peak), InnoDB buffer pool use and hit ratio, temporary tables on disk, and uptime.

## Users and roles

The list shows accounts and roles. **New user** and **New role** create one.

- **Account**: user name and host (`%` = any host, `localhost` = this machine, `10.0.%` = a network), authentication plugin, password, password expiry, lock, required encryption (`REQUIRE SSL`, `X509`, or issuer / subject / cipher), and limits (queries, updates and connections per hour, simultaneous connections, max statement time; `0` = no limit).
- **Roles**: grant roles, with **can grant it** (`WITH ADMIN OPTION`), and pick the default role.
- **Privileges**: tick privileges at the global, database, table, column or routine level.
- **Proxy accounts**: `GRANT PROXY` for authentication plugins that support it, such as PAM.

**Review changes** shows the `CREATE`, `ALTER`, `GRANT` and `REVOKE` statements before running them. They run one after the other and take effect at once.

## Replication

**Replication channels** lists each source this server replicates from: I/O and SQL threads, lag, position and GTID.

- **Start the channel** / **Stop the channel**.
- **Skip** skips the failing event(s) and restarts the SQL thread. Skipped events are never applied: the replica's data may then differ from the source.
- **New channel** / **Change the source** set up a channel (`CHANGE MASTER TO`, or `CHANGE REPLICATION SOURCE TO` on MySQL 8.0.26+) with a preview: source host, port, user, password, TLS, and where to start (GTID or file and position). The password is not kept by the app. Start the channel afterwards.

### Galera cluster

On a Galera node, the same screen shows the cluster: state, size, readiness, flow control (time paused, messages sent and received), receive and send queues, certification failures and brute-force aborts, provider and node addresses. Node names need the `SUPER` privilege and the `wsrep_info` plugin; otherwise only addresses are shown.

## Binlog

Lists the binary log files and their events (position, type, server, details). The binlog format and current GTID position are shown at the top.

- **Go** reads from a byte position. **Load more** reads further.
- **By transaction** groups events per transaction, with the tables and statements involved. Use this view to find row changes: the text filter does not search inside row events.
- `ANNOTATE_ROWS` events show the statement behind the row changes.
- **Undo selection (n)** generates the undo SQL of the selected transactions with `mariadb-binlog --flashback` and opens it in the SQL editor **without running it**. Read it, then run it yourself. It needs `binlog_format = ROW`, `binlog_row_image = FULL`, the `mariadb-binlog` client installed (see [Installation](installation.md#optional-mariadb-binlog)), and the `BINLOG REPLAY` privilege to run. Only `INSERT`, `UPDATE` and `DELETE` are reversed.
- **Delete the files before this one** runs `PURGE BINARY LOGS`. A replica that has not read them yet can no longer catch up.

## Plugins and engines

**Plugins** lists the server's plugins with their type, status, maturity and library. On MariaDB, plugins whose library is already on the server can be installed with one click (**Not installed only** shows them). On MySQL, type the plugin's name and its library file. Installed plugins stay installed after a restart.

**Engines** lists storage engines and whether they support transactions, XA and savepoints. MyRocks, Spider, CONNECT and ColumnStore appear once their plugin is installed.
