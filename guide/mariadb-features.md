# MariaDB features

These screens appear only on MariaDB servers.

## System-versioned tables

A system-versioned table keeps every past version of each row. Open the table's **Versioning** view.

- **Turn on versioning…** / **Turn off versioning…** run the `ALTER TABLE` after a preview. Turning it off deletes the history.
- The view shows the period columns, the number of history rows and the oldest version.
- **Purge history** deletes versions that ended before a chosen moment. Current rows are never touched.
- **History partitions**: split history by time interval or by row count, optionally with `AUTO` (MariaDB 10.9+) so new partitions are added by the server. A partition and the history it holds can be deleted.
- **Compare two moments** lists the rows added, removed or changed between two dates (needs a primary key).
- **Application-time periods**: link two date columns as a validity period, then use `UPDATE` / `DELETE … FOR PORTION OF`.

In the **Data** view of a versioned table:

- **As of** shows the table as it was at a moment (your local time). Pick the date with the calendar. Past data is read-only. **Back to current data** returns to now.
- **Row history** lists every version of the selected row.

All table changes show their SQL first. They take effect at once and cannot be rolled back.

## Sequences

A sequence tab shows the current state (next value not cached, cycles completed) and the options (start, minimum, maximum, increment, cache, cycle).

- **Apply changes…** runs `ALTER SEQUENCE`. **Restart with** sets a new start value.
- **Set the next value (SETVAL)** moves the sequence forward. `SETVAL` never goes backwards: use **Restart with** for that.
- **Use as a column default** gives the `DEFAULT NEXT VALUE FOR …` text to paste into a column definition.

## Vectors

The **Vectors** view of a table lists its `VECTOR` columns, their dimensions and their vector index (distance and `M`).

**Nearest neighbours** searches the `k` closest rows to a vector, typed as `[0.1, 0.2, …]` or taken from an existing row (by primary key). Choose Euclidean or cosine distance. Otaria warns when the search cannot use the index (no index, or an index built for the other distance): the whole table is then read.

Add a vector index in the table designer (**Indexes**).
