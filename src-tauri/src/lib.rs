mod browse;
mod db;
mod edit;
mod error;
mod profiles;
mod query;
mod ssh;

use std::collections::HashMap;

use serde::Serialize;
use tauri::{Manager, State};
use tokio::sync::Mutex;

use db::{ServerInfo, Session};
use error::{AppError, AppResult};
use profiles::{Profile, Secret, Store};

struct AppState {
    store: Store,
    sessions: Mutex<HashMap<String, Session>>,
}

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
    AppInfo { name: "Otaria", version: env!("CARGO_PKG_VERSION"), os: std::env::consts::OS }
}

#[tauri::command]
fn list_profiles(state: State<'_, AppState>) -> AppResult<Vec<Profile>> {
    state.store.list()
}

/// Save a profile. Secrets: `Some` = replace, `None` = keep as is.
/// When `save_password` is off, stored secrets are removed.
#[tauri::command]
fn save_profile(
    state: State<'_, AppState>,
    profile: Profile,
    password: Option<String>,
    ssh_secret: Option<String>,
) -> AppResult<Profile> {
    let saved = state.store.upsert(profile)?;
    for (kind, value) in [(Secret::Db, password), (Secret::Ssh, ssh_secret)] {
        match (saved.save_password, value) {
            (false, _) => state.store.set_secret(&saved.id, kind, None)?,
            (true, Some(v)) => state.store.set_secret(&saved.id, kind, Some(&v))?,
            (true, None) => {}
        }
    }
    Ok(saved)
}

#[tauri::command]
fn delete_profile(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.store.delete(&id)
}

/// Secrets typed in the form win; otherwise use the keychain (for saved profiles).
fn secrets(
    state: &AppState,
    profile: &Profile,
    password: Option<String>,
    ssh_secret: Option<String>,
) -> AppResult<(Option<String>, Option<String>)> {
    let saved = |kind| match profile.id.is_empty() || !profile.save_password {
        true => Ok(None),
        false => state.store.get_secret(&profile.id, kind),
    };
    Ok((
        match password { Some(p) => Some(p), None => saved(Secret::Db)? },
        match ssh_secret { Some(s) => Some(s), None => saved(Secret::Ssh)? },
    ))
}

/// Connect once with the form values and disconnect: the "Test" button.
#[tauri::command]
async fn test_connection(
    state: State<'_, AppState>,
    profile: Profile,
    password: Option<String>,
    ssh_secret: Option<String>,
) -> AppResult<ServerInfo> {
    let (pw, ssh) = secrets(&state, &profile, password, ssh_secret)?;
    let session = db::open(&profile, pw.as_deref(), ssh.as_deref()).await?;
    let info = session.info.clone();
    session.close().await;
    Ok(info)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenedSession {
    session_id: String,
    profile: Profile,
    info: ServerInfo,
}

/// Open a session for a saved profile; it stays open until `disconnect`.
#[tauri::command]
async fn connect(
    state: State<'_, AppState>,
    profile_id: String,
    password: Option<String>,
    ssh_secret: Option<String>,
) -> AppResult<OpenedSession> {
    let profile = state.store.get(&profile_id)?;
    let (pw, ssh) = secrets(&state, &profile, password, ssh_secret)?;
    let session = db::open(&profile, pw.as_deref(), ssh.as_deref()).await?;
    let info = session.info.clone();
    let session_id = uuid::Uuid::new_v4().to_string();
    state.sessions.lock().await.insert(session_id.clone(), session);
    Ok(OpenedSession { session_id, profile, info })
}

#[tauri::command]
async fn disconnect(state: State<'_, AppState>, session_id: String) -> AppResult<()> {
    let session = state.sessions.lock().await.remove(&session_id);
    match session {
        Some(s) => {
            s.close().await;
            Ok(())
        }
        None => Err(AppError::new("SESSION_NOT_FOUND", session_id)),
    }
}

/// Clone the session's pool so the sessions lock is not held during queries.
async fn pool(state: &AppState, session_id: &str) -> AppResult<mysql_async::Pool> {
    match state.sessions.lock().await.get(session_id) {
        Some(s) => Ok(s.pool.clone()),
        None => Err(AppError::new("SESSION_NOT_FOUND", session_id)),
    }
}

#[tauri::command]
async fn list_schemas(state: State<'_, AppState>, session_id: String) -> AppResult<Vec<String>> {
    browse::schemas(&pool(&state, &session_id).await?).await
}

#[tauri::command]
async fn list_objects(state: State<'_, AppState>, session_id: String, schema: String) -> AppResult<Vec<browse::DbObject>> {
    browse::objects(&pool(&state, &session_id).await?, &schema).await
}

#[tauri::command]
async fn table_info(state: State<'_, AppState>, session_id: String, schema: String, table: String) -> AppResult<browse::TableInfo> {
    browse::table_info(&pool(&state, &session_id).await?, &schema, &table).await
}

#[tauri::command]
async fn object_ddl(state: State<'_, AppState>, session_id: String, schema: String, name: String, kind: String) -> AppResult<String> {
    browse::ddl(&pool(&state, &session_id).await?, &schema, &name, &kind).await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn table_data(
    state: State<'_, AppState>,
    session_id: String,
    schema: String,
    table: String,
    filter: Option<String>,
    order: Vec<browse::Sort>,
    offset: u64,
    limit: u64,
) -> AppResult<browse::Rows> {
    let sql = browse::data_sql(&schema, &table, filter.as_deref(), &order, offset, limit);
    browse::data(&pool(&state, &session_id).await?, &sql).await
}

#[tauri::command]
fn preview_changes(schema: String, table: String, changes: Vec<edit::Change>) -> AppResult<Vec<String>> {
    edit::preview(&schema, &table, &changes)
}

#[tauri::command]
async fn apply_changes(
    state: State<'_, AppState>,
    session_id: String,
    schema: String,
    table: String,
    changes: Vec<edit::Change>,
) -> AppResult<u64> {
    edit::apply(&pool(&state, &session_id).await?, &schema, &table, &changes).await
}

/// Pool and connection of an editor tab, opened on first use.
async fn editor(state: &AppState, session_id: &str, editor_id: &str, schema: Option<&str>) -> AppResult<(mysql_async::Pool, u32, std::sync::Arc<Mutex<mysql_async::Conn>>)> {
    let pool = pool(state, session_id).await?;
    let found = {
        let sessions = state.sessions.lock().await;
        let eds = sessions.get(session_id).map(|s| s.editors.lock().unwrap());
        eds.and_then(|e| e.get(editor_id).map(|e| (e.conn_id, e.conn.clone())))
    };
    if let Some((id, conn)) = found {
        return Ok((pool, id, conn));
    }
    let ed = query::open_editor(&pool, schema).await?;
    let out = (pool, ed.conn_id, ed.conn.clone());
    match state.sessions.lock().await.get(session_id) {
        Some(s) => s.editors.lock().unwrap().insert(editor_id.to_string(), ed),
        None => return Err(AppError::new("SESSION_NOT_FOUND", session_id)),
    };
    Ok(out)
}

/// Run the script (or only the statement under `cursor`, a byte offset) in the editor's connection.
/// Destructive statements are refused with CONFIRM_RISKY until the UI sends `confirmed`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn run_sql(
    state: State<'_, AppState>,
    session_id: String,
    editor_id: String,
    schema: Option<String>,
    sql: String,
    cursor: Option<usize>,
    max_rows: usize,
    confirmed: bool,
) -> AppResult<query::RunOutput> {
    let stmts = query::pick(&sql, cursor);
    let risky: Vec<&str> = stmts.iter().map(String::as_str).filter(|s| query::risky(s)).collect();
    if !confirmed && !risky.is_empty() {
        return Err(AppError { detail: Some(risky.join(";\n")), ..AppError::new("CONFIRM_RISKY", "") });
    }
    let (_, _, conn) = editor(&state, &session_id, &editor_id, schema.as_deref()).await?;
    let mut conn = conn.lock().await;
    Ok(query::run(&mut conn, &stmts, max_rows).await)
}

#[tauri::command]
async fn cancel_sql(state: State<'_, AppState>, session_id: String, editor_id: String) -> AppResult<()> {
    let id = {
        let sessions = state.sessions.lock().await;
        let eds = sessions.get(&session_id).map(|s| s.editors.lock().unwrap());
        eds.and_then(|e| e.get(&editor_id).map(|e| e.conn_id))
    };
    match id {
        Some(id) => query::cancel(&pool(&state, &session_id).await?, id).await,
        None => Ok(()), // nothing ever ran in this tab
    }
}

#[tauri::command]
async fn close_editor(state: State<'_, AppState>, session_id: String, editor_id: String) -> AppResult<()> {
    if let Some(s) = state.sessions.lock().await.get(&session_id) {
        s.editors.lock().unwrap().remove(&editor_id);
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_config_dir()?;
            app.manage(AppState { store: Store::new(dir), sessions: Mutex::new(HashMap::new()) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            list_profiles,
            save_profile,
            delete_profile,
            test_connection,
            connect,
            disconnect,
            list_schemas,
            list_objects,
            table_info,
            object_ddl,
            table_data,
            preview_changes,
            apply_changes,
            run_sql,
            cancel_sql,
            close_editor
        ])
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
