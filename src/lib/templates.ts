import type { ObjectKind } from "@/lib/api";

const q = (s: string) => "`" + s.replace(/`/g, "``") + "`";

/** Kinds whose body contains `;`: wrapped in DELIMITER so the editor sends them as one statement. */
const BODY: ObjectKind[] = ["procedure", "function", "trigger", "event"];
const wrap = (kind: ObjectKind, sql: string) => (BODY.includes(kind) ? `DELIMITER $$\n${sql.trim()}$$\nDELIMITER ;\n` : `${sql.trim()};\n`);

/**
 * Turn `SHOW CREATE …` output into a script that replaces the object. MariaDB has CREATE OR REPLACE for
 * every kind; MySQL only for views, so there the old object is dropped first.
 */
export function editScript(kind: ObjectKind, schema: string, name: string, ddl: string, mariadb: boolean): string {
  if (mariadb || kind === "view") return wrap(kind, ddl.replace(/^\s*CREATE\s+/i, "CREATE OR REPLACE "));
  return `DROP ${kind.toUpperCase()} IF EXISTS ${q(schema)}.${q(name)};\n` + wrap(kind, ddl);
}

/** Starting point for a new object; runs in the editor with `schema` as the current database. */
export function newScript(kind: Exclude<ObjectKind, "table" | "sequence">): string {
  const sql = {
    view: "CREATE VIEW new_view AS\nSELECT 1 AS id",
    procedure: "CREATE PROCEDURE new_procedure(IN p_id INT)\nBEGIN\n  SELECT p_id;\nEND",
    function: "CREATE FUNCTION new_function(p_id INT) RETURNS INT\nDETERMINISTIC\nBEGIN\n  RETURN p_id;\nEND",
    trigger: "CREATE TRIGGER new_trigger\nBEFORE INSERT ON some_table\nFOR EACH ROW\nBEGIN\n  SET NEW.id = NEW.id;\nEND",
    event: "CREATE EVENT new_event\nON SCHEDULE EVERY 1 DAY\nDO\nBEGIN\n  DO 1;\nEND",
  }[kind];
  return wrap(kind, sql);
}
