# SQL editor

`Mod+T` (or **New query**) opens a query tab. Each tab has its own server session, so session variables and transactions stay in that tab.

## Running SQL

| Button | Keys | Runs |
|---|---|---|
| **Run** | `Mod+Enter`, `F9` | The whole script, or the selection |
| **Current** | `Mod+Shift+Enter` | The statement under the cursor |
| **Analyze** | | The `SELECT` under the cursor with `ANALYZE` (MariaDB) or `EXPLAIN ANALYZE` (MySQL) |
| **Stop** | | Cancels the running statement |

- Several statements give several **Result** tabs. **Messages** lists affected rows, warnings and timings.
- Routine bodies need `DELIMITER`, as in the `mariadb` command-line client:

  ```sql
  DELIMITER //
  CREATE PROCEDURE hello() BEGIN SELECT 'hello'; END //
  DELIMITER ;
  ```

- **Current database** picks the default database of the tab.
- **Max rows** caps how many rows are shown. When the cap is hit, **Count** runs the query again on the server to give the total.
- Statements that modify or delete data or objects (`DELETE`, `DROP`, `TRUNCATE`…) ask for confirmation first.

## Execution plans

**Analyze** really runs the query and shows the plan as a tree: real rows and times next to the optimizer's estimates. An estimate off by 10× or more is flagged; stale statistics are a common cause (`ANALYZE TABLE`). **Show raw result** shows the server's output as is.

## History and favorites

**History** keeps the queries you ran; click one to put it back in the editor. **Favorites** keeps named queries: **Save current** adds the editor's content.
