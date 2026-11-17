use serde::Serialize;

/// Basic facts about the running app, shown in the UI and used as an IPC smoke test.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    name: &'static str,
    version: &'static str,
    os: &'static str,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        name: "Otaria",
        version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![app_info])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_info_reports_crate_version() {
        let info = super::app_info();
        assert_eq!(info.name, "Otaria");
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
    }
}
