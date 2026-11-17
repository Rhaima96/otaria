import { invoke } from "@tauri-apps/api/core";

// Typed wrappers around Rust commands (src-tauri/src/lib.rs). Keep names in sync.

export interface AppInfo {
  name: string;
  version: string;
  os: string;
}

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
};
