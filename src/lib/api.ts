import { invoke } from "@tauri-apps/api/core";

// Typed wrappers around Rust commands (src-tauri/src/lib.rs). Keep names and shapes in sync.

export interface AppInfo {
  name: string;
  version: string;
  os: string;
}

export type TlsMode = "disabled" | "preferred" | "required" | "verify-ca" | "verify-identity";

export interface Profile {
  id: string;
  name: string;
  color: string | null;
  host: string;
  port: number;
  user: string;
  database: string | null;
  socket: string | null;
  savePassword: boolean;
  tls: { mode: TlsMode; ca: string | null; cert: string | null; key: string | null };
  ssh: { host: string; port: number; user: string; keyPath: string | null; hostKey: string | null } | null;
}

export interface ServerInfo {
  version: string;
  versionComment: string;
  isMariadb: boolean;
  currentUser: string;
  connectionId: number;
  tlsCipher: string | null;
  viaSsh: boolean;
}

export interface OpenedSession {
  sessionId: string;
  profile: Profile;
  info: ServerInfo;
}

/** Error from the backend: `code` is translated (errors.<code>), `message` is shown as-is. */
export interface AppError {
  code: string;
  message: string;
  serverCode: number | null;
  detail: string | null;
}

export function isAppError(e: unknown): e is AppError {
  return typeof e === "object" && e !== null && "code" in e && "message" in e;
}

export function toAppError(e: unknown): AppError {
  return isAppError(e) ? e : { code: "UNKNOWN", message: String(e), serverCode: null, detail: null };
}

export type ObjectKind = "table" | "view" | "sequence" | "procedure" | "function" | "trigger" | "event";

export interface DbObject {
  name: string;
  kind: ObjectKind;
  versioned: boolean;
}

export interface Column {
  name: string;
  dataType: string;
  columnType: string;
  nullable: boolean;
  default: string | null;
  extra: string;
  comment: string;
}

export interface TableInfo {
  columns: Column[];
  /** Empty = no primary key, the grid is read-only. */
  primaryKey: string[];
}

export interface Sort {
  column: string;
  desc: boolean;
}

export interface Rows {
  columns: string[];
  rows: (string | null)[][];
}

/** Column/value pairs; null = SQL NULL. */
export type Pairs = [string, string | null][];

export type Change =
  | { kind: "update"; key: Pairs; set: Pairs }
  | { kind: "insert"; values: Pairs }
  | { kind: "delete"; key: Pairs };

/** Secrets: undefined/null = keep the saved one (or none). */
type Secrets = { password?: string | null; sshSecret?: string | null };

/** Every version of one row; `start`/`end` name the period columns (UTC). */
export interface History {
  rows: Rows;
  start: string;
  end: string;
}

/** Sequence options; numbers are strings because BIGINT bounds do not fit in a JS number. */
export interface SeqOpts {
  start: string;
  min: string;
  max: string;
  increment: string;
  cache: string;
  cycle: boolean;
  /** ALTER only: next value to hand out, "" = unchanged. */
  restart: string;
}

export interface SeqState {
  opts: SeqOpts;
  nextNotCached: string;
  cycleCount: string;
}

// Table designer. `original` = name as loaded (null = new), so a rename is not a drop + add.
export type DefaultValue =
  | { kind: "none" }
  | { kind: "null" }
  | { kind: "literal"; value: string }
  | { kind: "expr"; value: string };

export interface ColumnDef {
  original: string | null;
  name: string;
  colType: string;
  nullable: boolean;
  default: DefaultValue;
  generated: { expr: string; stored: boolean } | null;
  autoIncrement: boolean;
  onUpdate: string | null;
  invisible: boolean;
  collation: string | null;
  check: string | null;
  comment: string;
}

export interface IndexDef {
  original: string | null;
  name: string;
  /** VECTOR: kept or dropped, never edited in the designer (its M / DISTANCE options are not loaded). */
  kind: "PRIMARY" | "UNIQUE" | "INDEX" | "FULLTEXT" | "SPATIAL" | "VECTOR";
  columns: { name: string; length: number | null; desc: boolean }[];
  comment: string;
  /** Not used by the optimizer: IGNORED (MariaDB) / INVISIBLE (MySQL). */
  ignored: boolean;
}

export interface FkDef {
  original: string | null;
  name: string;
  columns: string[];
  refSchema: string;
  refTable: string;
  refColumns: string[];
  onUpdate: string;
  onDelete: string;
}

export interface CheckDef {
  original: string | null;
  name: string;
  clause: string;
}

export interface TableDef {
  name: string;
  engine: string;
  collation: string;
  comment: string;
  columns: ColumnDef[];
  indexes: IndexDef[];
  fks: FkDef[];
  checks: CheckDef[];
}

/** instant → online (INPLACE, LOCK=NONE) → default (server's choice, may copy). */
export type Algorithm = "instant" | "online" | "default";

export interface StmtResult {
  sql: string;
  /** Empty = no result set (INSERT, SET…). */
  columns: string[];
  rows: (string | null)[][];
  /** Rows sent by the server; above rows.length when the display cap was hit. */
  totalRows: number;
  affectedRows: number;
  warnings: number;
  elapsedMs: number;
}

export interface RunOutput {
  results: StmtResult[];
  error: AppError | null;
  errorSql: string | null;
  schema: string | null;
}

export interface RunRequest {
  schema: string | null;
  sql: string;
  /** UTF-8 byte offset: run only the statement there. */
  cursor: number | null;
  maxRows: number;
  confirmed: boolean;
}

export interface Plugin {
  name: string;
  kind: string;
  status: string;
  /** null = built into the server. */
  library: string | null;
  maturity: string;
  description: string;
}

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
  listProfiles: () => invoke<Profile[]>("list_profiles"),
  saveProfile: (profile: Profile, s: Secrets = {}) => invoke<Profile>("save_profile", { profile, ...s }),
  deleteProfile: (id: string) => invoke<void>("delete_profile", { id }),
  testConnection: (profile: Profile, s: Secrets = {}) => invoke<ServerInfo>("test_connection", { profile, ...s }),
  connect: (profileId: string, s: Secrets = {}) => invoke<OpenedSession>("connect", { profileId, ...s }),
  disconnect: (sessionId: string) => invoke<void>("disconnect", { sessionId }),
  listSchemas: (sessionId: string) => invoke<string[]>("list_schemas", { sessionId }),
  listObjects: (sessionId: string, schema: string) => invoke<DbObject[]>("list_objects", { sessionId, schema }),
  tableInfo: (sessionId: string, schema: string, table: string) => invoke<TableInfo>("table_info", { sessionId, schema, table }),
  objectDdl: (sessionId: string, schema: string, name: string, kind: ObjectKind) =>
    invoke<string>("object_ddl", { sessionId, schema, name, kind }),
  tableData: (sessionId: string, schema: string, table: string, q: { filter: string; order: Sort[]; offset: number; limit: number; asOf: string | null }) =>
    invoke<Rows>("table_data", { sessionId, schema, table, ...q }),
  previewChanges: (schema: string, table: string, changes: Change[]) => invoke<string[]>("preview_changes", { schema, table, changes }),
  applyChanges: (sessionId: string, schema: string, table: string, changes: Change[]) =>
    invoke<number>("apply_changes", { sessionId, schema, table, changes }),
  rowHistory: (sessionId: string, schema: string, table: string, key: Pairs) => invoke<History>("row_history", { sessionId, schema, table, key }),
  sequenceState: (sessionId: string, schema: string, name: string) => invoke<SeqState>("sequence_state", { sessionId, schema, name }),
  sequenceDdl: (schema: string, name: string, opts: SeqOpts, create: boolean) => invoke<string>("sequence_ddl", { schema, name, opts, create }),
  sequenceApply: (sessionId: string, schema: string, name: string, opts: SeqOpts, create: boolean) =>
    invoke<void>("sequence_apply", { sessionId, schema, name, opts, create }),
  sequenceSetval: (sessionId: string, schema: string, name: string, value: string) =>
    invoke<string | null>("sequence_setval", { sessionId, schema, name, value }),
  tableDef: (sessionId: string, schema: string, table: string) => invoke<TableDef>("table_def", { sessionId, schema, table }),
  listEngines: (sessionId: string) => invoke<string[]>("list_engines", { sessionId }),
  listCollations: (sessionId: string) => invoke<string[]>("list_collations", { sessionId }),
  designerSql: (sessionId: string, schema: string, old: TableDef | null, next: TableDef, algorithm: Algorithm) =>
    invoke<string[]>("designer_sql", { sessionId, schema, old, new: next, algorithm }),
  designerApply: (sessionId: string, schema: string, old: TableDef | null, next: TableDef, algorithm: Algorithm) =>
    invoke<void>("designer_apply", { sessionId, schema, old, new: next, algorithm }),
  dropSql: (schema: string, name: string, kind: ObjectKind, truncate: boolean) => invoke<string>("drop_sql", { schema, name, kind, truncate }),
  dropObject: (sessionId: string, schema: string, name: string, kind: ObjectKind, truncate: boolean) =>
    invoke<void>("drop_object", { sessionId, schema, name, kind, truncate }),
  listPlugins: (sessionId: string) => invoke<Plugin[]>("list_plugins", { sessionId }),
  pluginInstallSql: (name: string, library: string) => invoke<string>("plugin_install_sql", { name, library }),
  pluginInstall: (sessionId: string, name: string, library: string) => invoke<void>("plugin_install", { sessionId, name, library }),
  runSql: (sessionId: string, editorId: string, r: RunRequest) => invoke<RunOutput>("run_sql", { sessionId, editorId, ...r }),
  cancelSql: (sessionId: string, editorId: string) => invoke<void>("cancel_sql", { sessionId, editorId }),
  closeEditor: (sessionId: string, editorId: string) => invoke<void>("close_editor", { sessionId, editorId }),
};

export function newProfile(): Profile {
  return {
    id: "",
    name: "",
    color: null,
    host: "127.0.0.1",
    port: 3306,
    user: "root",
    database: null,
    socket: null,
    savePassword: true,
    tls: { mode: "preferred", ca: null, cert: null, key: null },
    ssh: null,
  };
}
