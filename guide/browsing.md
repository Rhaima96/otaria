# Browsing and editing data

## Object tree

The left panel lists databases with their tables, views, sequences, procedures, functions, triggers and events. Click an object to open it in a tab. **Refresh** (or `F5` when no tab is active) reloads the tree.

**New database…** creates one. Next to each database, buttons give **Export as SQL…**, **Import an SQL file…**, **ER diagram…** and **Compare with…** (see [Import, export and compare](transfer.md)), and the database menu gives **Character set and collation…** (changes only the defaults for new tables) and **Drop database**.

**New…** creates a table (with the designer), a view, a sequence, a procedure, a function, a trigger or an event.

## Find an object

`Mod+P` opens a search over every object of every database. Type part of a name (`schema.name` works too), move with `↑` `↓`, open with `Enter`. Results also include matching columns, and routine bodies when **Also search routine bodies** is ticked.

## Object tabs

A table tab has several views:

- **Data**: the data grid.
- **Structure**: columns, types, defaults, keys, and the table designer.
- **DDL**: the `CREATE` statement. **Edit in the SQL editor** opens it in a query tab.
- **Versioning** and **Vectors**: see [MariaDB features](mariadb-features.md).

**Empty** (`TRUNCATE`) and **Delete** (`DROP`) ask you to type the object's name to confirm.

## Data grid

Rows load in pages; **Load more** reads the next page. The **Filter** box takes a `WHERE` condition, for example `id > 100`.

Editing:

1. Double-click a cell to edit it. **Set NULL** and **(default)** are available for any cell. **Add row** and **Delete** work on whole rows.
2. Changed cells are highlighted. **Discard** cancels them.
3. **Save (n)** shows the SQL to run. **Run** applies it in one transaction: if one statement fails, nothing is changed.

A table without a primary key is **read-only** in the grid.

Long values are cut at 1024 characters in the grid. Open the cell with the **JSON** button to read or edit it whole.

### JSON cells

The **JSON** button opens the focused cell in a tree view or as text, with **Format** and validation. **Apply to cell** puts the result back in the grid (saved with the other changes). For a JSON array, **JSON_TABLE** opens a query that turns it into rows.

## Table designer

The designer creates a table or changes an existing one. Tabs:

- **Columns**: type, nullable, default (none, text or expression), auto increment, generated columns (virtual or stored), `ON UPDATE`, collation, check, invisible, comment, and per-column versioning.
- **Indexes**: kinds, columns, and **Ignored** indexes (kept up to date but unused by the optimizer: a safe way to test dropping an index).
- **Foreign keys**: referenced table and columns, `ON DELETE`, `ON UPDATE`.
- **Checks**: named `CHECK` constraints.
- **Options**: engine, system versioning, application periods, `WITHOUT OVERLAPS`, partitioning (`PARTITION BY` as SQL).

**Apply…** shows the `ALTER TABLE` before running it. Otaria first tries `ALGORITHM=INSTANT`. If the server refuses, you choose between **Run online** (`ALGORITHM=INPLACE, LOCK=NONE`: the table stays readable and writable) and **Run anyway** (may block writes). You can also choose how long to wait for the table lock (`WAIT n` or `NOWAIT`). Long changes show their progress and can be stopped.
