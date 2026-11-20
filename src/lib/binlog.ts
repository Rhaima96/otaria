import type { BinlogEvent } from "@/lib/api";

/** One transaction of the binary log: from its Gtid event to the Xid / COMMIT that closes it. */
export interface Txn {
  /** `0-1-12` (MariaDB domain-server-sequence), `uuid:n` (MySQL), "" for a MySQL anonymous transaction. */
  gtid: string;
  pos: number;
  events: BinlogEvent[];
  /** `db.table` of its Table_map events. */
  tables: string[];
  /** Statements: Annotate_rows / Rows_query (behind row events), or the Query of a DDL. */
  statements: string[];
}

/**
 * Loaded events grouped by transaction. MariaDB: `BEGIN GTID x` opens a transaction closed by Xid or COMMIT;
 * a bare `GTID x` (DDL) holds the one Query after it. MySQL: a Gtid / Anonymous_Gtid event
 * (`SET @@SESSION.GTID_NEXT= 'x'`), then either a `BEGIN` Query (closed by Xid or COMMIT) or the DDL Query. Events before the first Gtid of the page
 * (format description, a transaction cut by the page start) belong to none.
 */
export function transactions(events: BinlogEvent[]): Txn[] {
  const out: Txn[] = [];
  let cur: Txn | null = null;
  let single = false;
  for (const e of events) {
    if (e.kind === "Gtid" || e.kind === "Anonymous_Gtid") {
      const next = /GTID_NEXT\s*=\s*'([^']*)'/i.exec(e.info)?.[1];
      single = !/^BEGIN\b/i.test(e.info);
      cur = { gtid: next !== undefined ? next.replace(/^ANONYMOUS$/i, "") : e.info.replace(/^(BEGIN\s+)?GTID\s+/i, "").trim(), pos: e.pos, events: [], tables: [], statements: [] };
      out.push(cur);
    }
    if (!cur) continue;
    cur.events.push(e);
    // MySQL: the BEGIN right after the Gtid event makes it a multi-event transaction.
    if (e.kind === "Query" && cur.events.length === 2 && /^\s*BEGIN\b/i.test(e.info)) {
      single = false;
      continue;
    }
    const table = e.kind === "Table_map" ? /\(([^)]+)\)\s*$/.exec(e.info)?.[1] : undefined;
    if (table && !cur.tables.includes(table)) cur.tables.push(table);
    const commit = e.kind === "Xid" || (e.kind === "Query" && /^\s*COMMIT\b/i.test(e.info));
    if (e.kind === "Rows_query") cur.statements.push(e.info.replace(/^#\s*/, ""));
    if (e.kind === "Annotate_rows" || (e.kind === "Query" && !commit && !/^\s*BEGIN\b/i.test(e.info))) cur.statements.push(e.info);
    if (commit || (single && e.kind === "Query")) cur = null;
  }
  return out;
}
