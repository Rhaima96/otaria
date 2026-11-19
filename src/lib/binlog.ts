import type { BinlogEvent } from "@/lib/api";

/** One transaction of the binary log: from its Gtid event to the Xid / COMMIT that closes it. */
export interface Txn {
  /** `0-1-12` (domain-server-sequence). */
  gtid: string;
  pos: number;
  events: BinlogEvent[];
  /** `db.table` of its Table_map events. */
  tables: string[];
  /** Statements: Annotate_rows (behind row events), or the Query of a DDL. */
  statements: string[];
}

/**
 * Loaded events grouped by transaction. `BEGIN GTID x` opens a transaction closed by Xid or COMMIT;
 * a bare `GTID x` (DDL) holds the one Query after it. Events before the first Gtid of the page
 * (format description, a transaction cut by the page start) belong to none.
 */
export function transactions(events: BinlogEvent[]): Txn[] {
  const out: Txn[] = [];
  let cur: Txn | null = null;
  let single = false;
  for (const e of events) {
    if (e.kind === "Gtid") {
      single = !/^BEGIN\b/i.test(e.info);
      cur = { gtid: e.info.replace(/^(BEGIN\s+)?GTID\s+/i, "").trim(), pos: e.pos, events: [], tables: [], statements: [] };
      out.push(cur);
    }
    if (!cur) continue;
    cur.events.push(e);
    const table = e.kind === "Table_map" ? /\(([^)]+)\)\s*$/.exec(e.info)?.[1] : undefined;
    if (table && !cur.tables.includes(table)) cur.tables.push(table);
    const commit = e.kind === "Xid" || (e.kind === "Query" && /^\s*COMMIT\b/i.test(e.info));
    if (e.kind === "Annotate_rows" || (e.kind === "Query" && !commit && !/^\s*BEGIN\b/i.test(e.info))) cur.statements.push(e.info);
    if (commit || (single && e.kind === "Query")) cur = null;
  }
  return out;
}
