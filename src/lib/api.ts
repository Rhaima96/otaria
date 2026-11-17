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
  tableData: (sessionId: string, schema: string, table: string, q: { filter: string; order: Sort[]; offset: number; limit: number }) =>
    invoke<Rows>("table_data", { sessionId, schema, table, ...q }),
  previewChanges: (schema: string, table: string, changes: Change[]) => invoke<string[]>("preview_changes", { schema, table, changes }),
  applyChanges: (sessionId: string, schema: string, table: string, changes: Change[]) =>
    invoke<number>("apply_changes", { sessionId, schema, table, changes }),
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
