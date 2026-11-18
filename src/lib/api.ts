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

export interface Period {
  name: string;
  start: string;
  end: string;
}

export interface VersioningState {
  versioned: boolean;
  system: Period | null;
  implicit: boolean;
  periods: Period[];
  /** SYSTEM_TIME partitions, current one (description "CURRENT") last. */
  partitions: { name: string; description: string | null; rows: number }[];
  otherPartitioning: boolean;
  historyRows: number;
  /** UTC. */
  oldest: string | null;
}

export type VersioningAction =
  | { kind: "enable" }
  | { kind: "disable" }
  | { kind: "purge"; before: string }
  | { kind: "partition"; interval: boolean; n: number; unit: string; auto: boolean; partitions: number | null }
  | { kind: "unpartition" }
  | { kind: "dropPartition"; name: string }
  | { kind: "addPeriod"; name: string; start: string; end: string }
  | { kind: "dropPeriod"; name: string };

export interface VersionDiff {
  columns: string[];
  rows: { kind: "added" | "removed" | "changed"; before: (string | null)[] | null; after: (string | null)[] | null }[];
  truncated: boolean;
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
  /** VECTOR: MariaDB 11.7+, one VECTOR NOT NULL column. */
  kind: "PRIMARY" | "UNIQUE" | "INDEX" | "FULLTEXT" | "SPATIAL" | "VECTOR";
  columns: { name: string; length: number | null; desc: boolean }[];
  comment: string;
  /** Not used by the optimizer: IGNORED (MariaDB) / INVISIBLE (MySQL). */
  ignored: boolean;
  /** VECTOR only: graph neighbours (3–200) and metric; null = server default. */
  m?: number | null;
  distance?: "euclidean" | "cosine" | null;
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
  /** Run the statement as ANALYZE FORMAT=JSON (one SELECT only). */
  analyze?: boolean;
}

export interface VectorColumn {
  name: string;
  dimensions: number;
  index: { name: string; m: number | null; distance: string | null } | null;
}

export type VectorDistance = "euclidean" | "cosine";
/** Compare with a typed vector, or with the vector of the row whose (single-column) primary key is `value`. */
export type VectorProbe = { kind: "text"; value: string } | { kind: "row"; value: string };

export interface BinlogState {
  /** ROW | STATEMENT | MIXED */
  format: string;
  /** Oldest first; the last one is being written. */
  files: { name: string; size: number }[];
  /** @@gtid_binlog_pos */
  gtid: string;
}

export interface BinlogEvent {
  pos: number;
  kind: string;
  serverId: number;
  endPos: number;
  info: string;
}

export interface Engine {
  name: string;
  /** YES | NO | DEFAULT | DISABLED */
  support: string;
  comment: string;
  transactions: boolean;
  xa: boolean;
  savepoints: boolean;
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

export interface Process {
  id: number;
  user: string;
  host: string;
  db: string | null;
  command: string;
  timeMs: number;
  state: string | null;
  info: string | null;
  /** 0–100, MariaDB only. */
  progress: number;
}

export interface Variable {
  name: string;
  value: string | null;
  /** The fields below are null on MySQL. */
  default: string | null;
  /** INT, BIGINT UNSIGNED, DOUBLE, BOOLEAN, ENUM, SET, FLAGSET, VARCHAR… */
  kind: string | null;
  origin: string | null;
  readOnly: boolean;
  comment: string | null;
  /** Comma-separated, for ENUM / SET / FLAGSET / BOOLEAN. */
  choices: string | null;
}

export interface Account {
  user: string;
  host: string;
  isRole: boolean;
  plugin: string;
  locked: boolean;
  expired: boolean;
  /** Days: null = server default, 0 = never. */
  lifetime: number | null;
  maxQuestions: number;
  maxUpdates: number;
  maxConnections: number;
  maxUserConnections: number;
  /** Seconds, MariaDB only. */
  maxStatementTime: number;
  defaultRole: string | null;
  roles: string[];
}

/** What the account editor sends: original = [user, host] being edited, null to create. password null = unchanged. */
export interface AccountForm extends Account {
  original: [string, string] | null;
  password: string | null;
  expireNow: boolean;
}

/** All null = global; then database, table, column. */
export interface GrantTarget {
  schema: string | null;
  table: string | null;
  column: string | null;
}

export interface Grant {
  target: GrantTarget;
  privileges: string[];
}

export interface Privilege {
  name: string;
  global: boolean;
  schema: boolean;
  table: boolean;
  column: boolean;
}

export interface PrivChange {
  target: GrantTarget;
  grant: string[];
  revoke: string[];
}

export type ExportFormat = "csv" | "json" | "xlsx" | "sql";

export interface ExportReq {
  schema: string;
  /** null = the whole schema (SQL only). */
  table: string | null;
  format: ExportFormat;
  path: string;
  data: boolean;
  drop: boolean;
}

export interface CsvPreview {
  /** Header names, or "1", "2"… without a header row. */
  columns: string[];
  rows: string[][];
  delimiter: string;
}

export interface CsvImport {
  schema: string;
  table: string;
  path: string;
  delimiter: string;
  header: boolean;
  /** Target column per CSV column, in file order; null = skip. */
  mapping: (string | null)[];
  emptyNull: boolean;
}

export interface CopyReq {
  schema: string;
  table: string | null;
  targetSession: string;
  targetSchema: string;
  data: boolean;
  drop: boolean;
}

/** Payload of the `transfer-progress` event. Export: rows so far (total 0); import: bytes read of total. */
export interface TransferProgress {
  id: string;
  done: number;
  total: number;
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
  versioningState: (sessionId: string, schema: string, table: string) => invoke<VersioningState>("versioning_state", { sessionId, schema, table }),
  versioningSql: (sessionId: string, schema: string, table: string, action: VersioningAction) => invoke<string[]>("versioning_sql", { sessionId, schema, table, action }),
  versioningApply: (sessionId: string, schema: string, table: string, action: VersioningAction) => invoke<void>("versioning_apply", { sessionId, schema, table, action }),
  versioningCompare: (sessionId: string, schema: string, table: string, key: string[], a: string, b: string) =>
    invoke<VersionDiff>("versioning_compare", { sessionId, schema, table, key, a, b }),
  portionSql: (schema: string, table: string, period: string) => invoke<string>("portion_sql", { schema, table, period }),
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
  erDiagram: (sessionId: string, schema: string, tables: string[], neighbors: boolean) => invoke<string>("er_diagram", { sessionId, schema, tables, neighbors }),
  saveDiagram: (path: string, text: string) => invoke<void>("save_diagram", { path, text }),
  vectorColumns: (sessionId: string, schema: string, table: string) => invoke<VectorColumn[]>("vector_columns", { sessionId, schema, table }),
  vectorSearch: (sessionId: string, schema: string, table: string, column: string, distance: VectorDistance, probe: VectorProbe, k: number) =>
    invoke<Rows>("vector_search", { sessionId, schema, table, column, distance, probe, k }),
  binlogState: (sessionId: string) => invoke<BinlogState>("binlog_state", { sessionId }),
  binlogEvents: (sessionId: string, file: string, offset: number, limit: number) => invoke<BinlogEvent[]>("binlog_events", { sessionId, file, offset, limit }),
  engineList: (sessionId: string) => invoke<Engine[]>("engine_list", { sessionId }),
  listPlugins: (sessionId: string) => invoke<Plugin[]>("list_plugins", { sessionId }),
  pluginInstallSql: (name: string, library: string) => invoke<string>("plugin_install_sql", { name, library }),
  pluginInstall: (sessionId: string, name: string, library: string) => invoke<void>("plugin_install", { sessionId, name, library }),
  listProcesses: (sessionId: string) => invoke<Process[]>("list_processes", { sessionId }),
  killProcess: (sessionId: string, id: number, connection: boolean) => invoke<void>("kill_process", { sessionId, id, connection }),
  explainProcess: (sessionId: string, id: number) => invoke<string | null>("explain_process", { sessionId, id }),
  listVariables: (sessionId: string) => invoke<Variable[]>("list_variables", { sessionId }),
  /** value null = back to the default. */
  variableSql: (name: string, kind: string | null, value: string | null) => invoke<string>("variable_sql", { name, kind, value }),
  setVariable: (sessionId: string, name: string, value: string | null) => invoke<void>("set_variable", { sessionId, name, value }),
  serverStatus: (sessionId: string) => invoke<Record<string, string>>("server_status", { sessionId }),
  listAccounts: (sessionId: string) => invoke<Account[]>("list_accounts", { sessionId }),
  accountGrants: (sessionId: string, user: string, host: string, isRole: boolean) => invoke<Grant[]>("account_grants", { sessionId, user, host, isRole }),
  /** [privileges from SHOW PRIVILEGES, active authentication plugins]. */
  usersMeta: (sessionId: string) => invoke<[Privilege[], string[]]>("users_meta", { sessionId }),
  /** Preview, password masked. */
  accountSql: (sessionId: string, form: AccountForm) => invoke<string[]>("account_sql", { sessionId, form }),
  accountApply: (sessionId: string, form: AccountForm) => invoke<void>("account_apply", { sessionId, form }),
  privilegesSql: (user: string, host: string, isRole: boolean, changes: PrivChange[]) => invoke<string[]>("privileges_sql", { user, host, isRole, changes }),
  privilegesApply: (sessionId: string, user: string, host: string, isRole: boolean, changes: PrivChange[]) =>
    invoke<void>("privileges_apply", { sessionId, user, host, isRole, changes }),
  accountDropSql: (user: string, host: string, isRole: boolean) => invoke<string>("account_drop_sql", { user, host, isRole }),
  accountDrop: (sessionId: string, user: string, host: string, isRole: boolean) => invoke<void>("account_drop", { sessionId, user, host, isRole }),
  exportData: (sessionId: string, jobId: string, req: ExportReq) => invoke<number>("export_data", { sessionId, jobId, req }),
  importSql: (sessionId: string, jobId: string, schema: string, path: string) => invoke<number>("import_sql", { sessionId, jobId, schema, path }),
  csvPreview: (path: string, delimiter: string | null, header: boolean) => invoke<CsvPreview>("csv_preview", { path, delimiter, header }),
  importCsv: (sessionId: string, jobId: string, req: CsvImport) => invoke<number>("import_csv", { sessionId, jobId, req }),
  copyData: (sessionId: string, jobId: string, req: CopyReq) => invoke<number>("copy_data", { sessionId, jobId, req }),
  cancelJob: (sessionId: string, jobId: string) => invoke<void>("cancel_job", { sessionId, jobId }),
  runSql: (sessionId: string, editorId: string, r: RunRequest) => invoke<RunOutput>("run_sql", { sessionId, editorId, analyze: false, ...r }),
  jsonTableSql: (schema: string, table: string, column: string, path: (string | number)[], sample: unknown) =>
    invoke<string>("json_table_sql", { schema, table, column, path, sample }),
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
