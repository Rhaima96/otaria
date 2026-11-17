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

/** Secrets: undefined/null = keep the saved one (or none). */
type Secrets = { password?: string | null; sshSecret?: string | null };

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
  listProfiles: () => invoke<Profile[]>("list_profiles"),
  saveProfile: (profile: Profile, s: Secrets = {}) => invoke<Profile>("save_profile", { profile, ...s }),
  deleteProfile: (id: string) => invoke<void>("delete_profile", { id }),
  testConnection: (profile: Profile, s: Secrets = {}) => invoke<ServerInfo>("test_connection", { profile, ...s }),
  connect: (profileId: string, s: Secrets = {}) => invoke<OpenedSession>("connect", { profileId, ...s }),
  disconnect: (sessionId: string) => invoke<void>("disconnect", { sessionId }),
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
