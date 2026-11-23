# Import, export and compare

## Export

**Export** on a table writes:

- **CSV**: optionally with `NULL` written as `\N`, so it differs from an empty text.
- **JSON**
- **Excel (.xlsx)**
- **SQL**: `CREATE` and, with **Include the rows**, `INSERT` statements. **Add DROP … IF EXISTS** makes the script replace existing objects. The script names no database, so it loads into any database.

**Export as SQL…** on a database exports every object in it.

Instead of a file, the destination can be **Another connection**: the table is copied to another open connection, or another database of the same one. The target database is created if it does not exist.

## Import

**Import an SQL file…** (button next to the database in the tree) runs a script in that database. Statements run one after the other; the first error stops the import, and the statements before it stay applied.

**Import CSV** (table) loads a CSV file into the table:

1. Choose the file, the delimiter (comma, semicolon, tab, pipe) and the encoding (UTF-8 or Latin-1 / Windows-1252).
2. Tick **First line = column names** if it is a header, and **Empty field = NULL** if needed.
3. Match each CSV column to a table column, or **skip** it. The first value of each column helps.
4. **Import** inserts all rows in one transaction: one bad row and nothing is imported.

## Compare two databases

**Compare with…** (button next to the database in the tree) compares this database with a source database, from the same or another open connection. It covers tables, sequences, views, routines, triggers and events, and lists objects **to create**, **to change** and **extra**.

**Open in the editor** puts the migration script in a query tab. It turns this database into the source. Nothing runs until you run it. `DROP` lines are commented out. Sequences change through `ALTER SEQUENCE` and keep their current value.

## ER diagram

**ER diagram…** (button next to the database in the tree) draws the tables of a database and their foreign keys. Pick tables with the filter, **All**, **None**, or **Add tables linked by a foreign key**. Zoom, fit to width, and save as **SVG** or **PNG**.

The **Mermaid text** view gives the same diagram as text: **Copy** it, or **Save .mmd…**. GitHub, GitLab, Notion and Obsidian render it as is.
