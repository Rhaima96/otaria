import { Braces, CalendarClock, Eye, History, ListOrdered, SquareFunction, Table, Zap } from "lucide-vue-next";
import type { DbObject, ObjectKind } from "@/lib/api";

/** Tree order: tables first, then views, sequences, routines, triggers, events. */
export const KIND_ORDER: ObjectKind[] = ["table", "view", "sequence", "procedure", "function", "trigger", "event"];

const ICONS = { table: Table, view: Eye, sequence: ListOrdered, procedure: Braces, function: SquareFunction, trigger: Zap, event: CalendarClock };

export function objectIcon(o: DbObject) {
  return o.versioned ? History : ICONS[o.kind];
}

/** Objects that have rows (Data and Structure tabs); the others only have DDL. */
export function hasRows(kind: ObjectKind) {
  return kind === "table" || kind === "view" || kind === "sequence";
}
