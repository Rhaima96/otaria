# Otaria user guide

Otaria is a desktop client built for MariaDB. It also connects to MySQL 5.7 to 8.4: the basic features work there and the MariaDB-only screens are hidden.

1. [Installation and updates](installation.md)
2. [Connections](connections.md): TLS, SSH tunnels, saved passwords
3. [Browsing and editing data](browsing.md): object tree, data grid, JSON, table designer
4. [SQL editor](sql-editor.md): running scripts, results, EXPLAIN ANALYZE, history
5. [MariaDB features](mariadb-features.md): system-versioned tables, sequences, vectors
6. [Server administration](administration.md): processes, variables, users, replication, Galera, binlog
7. [Import, export and compare](transfer.md): CSV, JSON, Excel, SQL, copy between servers, schema compare, ER diagrams

## Keyboard shortcuts

`Mod` is `⌘` on macOS and `Ctrl` on Windows and Linux. Press `?` in the app to see this list.

| Keys | Action |
|---|---|
| `Mod+P` | Find an object or a column |
| `Mod+T` | New query |
| `Mod+W` | Close the tab |
| `Mod+1` … `Mod+9` | Go to tab 1 to 9 (9 = last) |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `F5` | Reload the grid, or the tree |
| `Mod+Enter` or `F9` | Run the script, or the selection |
| `Mod+Shift+Enter` | Run the statement under the cursor |
| `?` or `Mod+/` | Show the shortcut list |

## Settings

Theme (system, light, dark) and language (English, Français, Español, Italiano, Deutsch) are in the app's header.
