mod admin;
mod browse;
mod db;
mod designer;
mod diagram;
mod edit;
mod error;
mod plugins;
mod profiles;
mod query;
mod sequence;
mod ssh;
mod transfer;
mod users;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;

use db::{ServerInfo, Session};
use error::{AppError, AppResult};
use profiles::{Profile, Secret, Store};

struct AppState {
    store: Store,
    sessions: Mutex<HashMap<String, Session>>,
    /// Running imports / exports by job id: cancel flag, and the connection to KILL QUERY (0 = none).
    jobs: std::sync::Mutex<HashMap<String, (Arc<AtomicBool>, u32)>>,
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
    as_of: Option<String>,
) -> AppResult<browse::Rows> {
    let sql = browse::data_sql(&schema, &table, filter.as_deref(), &order, offset, limit, as_of.is_some());
    let vars = match as_of {
        Some(t) => vec![(browse::AS_OF_VAR, Some(t))],
        None => vec![],
    };
    browse::data(&pool(&state, &session_id).await?, &sql, &vars).await
}

/// MariaDB or MySQL: some metadata and DDL keywords differ.
async fn is_mariadb(state: &AppState, session_id: &str) -> AppResult<bool> {
    match state.sessions.lock().await.get(session_id) {
        Some(s) => Ok(s.info.is_mariadb),
        None => Err(AppError::new("SESSION_NOT_FOUND", session_id)),
    }
}

#[tauri::command]
async fn table_def(state: State<'_, AppState>, session_id: String, schema: String, table: String) -> AppResult<designer::TableDef> {
    let mariadb = is_mariadb(&state, &session_id).await?;
    designer::load(&pool(&state, &session_id).await?, &schema, &table, mariadb).await
}

#[tauri::command]
async fn list_engines(state: State<'_, AppState>, session_id: String) -> AppResult<Vec<String>> {
    designer::engines(&pool(&state, &session_id).await?).await
}

/// Statements for the designer: CREATE TABLE when `old` is None, else RENAME + one ALTER TABLE.
fn designer_stmts(
    schema: &str,
    old: Option<&designer::TableDef>,
    new: &designer::TableDef,
    algorithm: designer::Algorithm,
    mariadb: bool,
) -> AppResult<Vec<String>> {
    match old {
        None => Ok(vec![designer::create_sql(schema, new, mariadb)?]),
        Some(old) => designer::alter_sql(schema, old, new, algorithm, mariadb),
    }
}

#[tauri::command]
async fn designer_sql(
    state: State<'_, AppState>,
    session_id: String,
    schema: String,
    old: Option<designer::TableDef>,
    new: designer::TableDef,
    algorithm: designer::Algorithm,
) -> AppResult<Vec<String>> {
    designer_stmts(&schema, old.as_ref(), &new, algorithm, is_mariadb(&state, &session_id).await?)
}

/// Collation names for the designer's suggestions.
#[tauri::command]
async fn list_collations(state: State<'_, AppState>, session_id: String) -> AppResult<Vec<String>> {
    designer::collations(&pool(&state, &session_id).await?).await
}

/// Rebuilt from the same inputs as the preview, so what runs is what was shown.
#[tauri::command]
async fn designer_apply(
    state: State<'_, AppState>,
    session_id: String,
    schema: String,
    old: Option<designer::TableDef>,
    new: designer::TableDef,
    algorithm: designer::Algorithm,
) -> AppResult<()> {
    let stmts = designer_stmts(&schema, old.as_ref(), &new, algorithm, is_mariadb(&state, &session_id).await?)?;
    designer::run(&pool(&state, &session_id).await?, &stmts).await
}

#[tauri::command]
fn drop_sql(schema: String, name: String, kind: String, truncate: bool) -> AppResult<String> {
    designer::drop_sql(&schema, &name, &kind, truncate)
}

#[tauri::command]
async fn drop_object(state: State<'_, AppState>, session_id: String, schema: String, name: String, kind: String, truncate: bool) -> AppResult<()> {
    let sql = designer::drop_sql(&schema, &name, &kind, truncate)?;
    designer::run(&pool(&state, &session_id).await?, &[sql]).await
}

#[tauri::command]
async fn row_history(state: State<'_, AppState>, session_id: String, schema: String, table: String, key: edit::Pairs) -> AppResult<browse::History> {
    browse::row_history(&pool(&state, &session_id).await?, &schema, &table, &key).await
}

#[tauri::command]
async fn sequence_state(state: State<'_, AppState>, session_id: String, schema: String, name: String) -> AppResult<sequence::SeqState> {
    sequence::state(&pool(&state, &session_id).await?, &schema, &name).await
}

/// The CREATE / ALTER SEQUENCE statement, shown before it runs.
#[tauri::command]
fn sequence_ddl(schema: String, name: String, opts: sequence::SeqOpts, create: bool) -> AppResult<String> {
    sequence::ddl(&schema, &name, &opts, create)
}

/// Rebuilds the statement from the same inputs as the preview, so what runs is what was shown.
#[tauri::command]
async fn sequence_apply(state: State<'_, AppState>, session_id: String, schema: String, name: String, opts: sequence::SeqOpts, create: bool) -> AppResult<()> {
    let sql = sequence::ddl(&schema, &name, &opts, create)?;
    sequence::apply(&pool(&state, &session_id).await?, &sql).await
}

#[tauri::command]
async fn sequence_setval(state: State<'_, AppState>, session_id: String, schema: String, name: String, value: String) -> AppResult<Option<String>> {
    sequence::setval(&pool(&state, &session_id).await?, &schema, &name, &value).await
}

#[tauri::command]
async fn list_processes(state: State<'_, AppState>, session_id: String) -> AppResult<Vec<admin::Process>> {
    admin::processes(&pool(&state, &session_id).await?, is_mariadb(&state, &session_id).await?).await
}

#[tauri::command]
async fn kill_process(state: State<'_, AppState>, session_id: String, id: u64, connection: bool) -> AppResult<()> {
    admin::kill(&pool(&state, &session_id).await?, id, connection).await
}

#[tauri::command]
async fn explain_process(state: State<'_, AppState>, session_id: String, id: u64) -> AppResult<Option<String>> {
    admin::explain(&pool(&state, &session_id).await?, id).await
}

#[tauri::command]
async fn list_variables(state: State<'_, AppState>, session_id: String) -> AppResult<Vec<admin::Variable>> {
    admin::variables(&pool(&state, &session_id).await?, is_mariadb(&state, &session_id).await?).await
}

#[tauri::command]
fn variable_sql(name: String, kind: Option<String>, value: Option<String>) -> AppResult<String> {
    admin::set_sql(&name, kind.as_deref(), value.as_deref())
}

#[tauri::command]
async fn set_variable(state: State<'_, AppState>, session_id: String, name: String, value: Option<String>) -> AppResult<()> {
    admin::set(&pool(&state, &session_id).await?, is_mariadb(&state, &session_id).await?, &name, value.as_deref()).await
}

#[tauri::command]
async fn server_status(state: State<'_, AppState>, session_id: String) -> AppResult<std::collections::HashMap<String, String>> {
    admin::status(&pool(&state, &session_id).await?).await
}

#[tauri::command]
async fn list_accounts(state: State<'_, AppState>, session_id: String) -> AppResult<Vec<users::Account>> {
    users::accounts(&pool(&state, &session_id).await?, is_mariadb(&state, &session_id).await?).await
}

#[tauri::command]
async fn account_grants(state: State<'_, AppState>, session_id: String, user: String, host: String, is_role: bool) -> AppResult<Vec<users::Grant>> {
    users::grants(&pool(&state, &session_id).await?, &user, &host, is_role).await
}

/// Privileges from SHOW PRIVILEGES and the active authentication plugins.
#[tauri::command]
async fn users_meta(state: State<'_, AppState>, session_id: String) -> AppResult<(Vec<users::Privilege>, Vec<String>)> {
    let pool = pool(&state, &session_id).await?;
    Ok((users::privileges(&pool).await?, users::auth_plugins(&pool).await?))
}

/// Password masked: this is what the preview shows.
#[tauri::command]
async fn account_sql(state: State<'_, AppState>, session_id: String, form: users::AccountForm) -> AppResult<Vec<String>> {
    let mariadb = is_mariadb(&state, &session_id).await?;
    let old = users::load(&pool(&state, &session_id).await?, mariadb, &form.original).await?;
    users::account_sql(old.as_ref(), &form, mariadb, users::Secret::Masked)
}

#[tauri::command]
async fn account_apply(state: State<'_, AppState>, session_id: String, form: users::AccountForm) -> AppResult<()> {
    users::apply_account(&pool(&state, &session_id).await?, is_mariadb(&state, &session_id).await?, &form).await
}

#[tauri::command]
fn privileges_sql(user: String, host: String, is_role: bool, changes: Vec<users::PrivChange>) -> AppResult<Vec<String>> {
    users::privileges_sql(&user, &host, is_role, &changes)
}

#[tauri::command]
async fn privileges_apply(state: State<'_, AppState>, session_id: String, user: String, host: String, is_role: bool, changes: Vec<users::PrivChange>) -> AppResult<()> {
    let sql = users::privileges_sql(&user, &host, is_role, &changes)?;
    users::run(&pool(&state, &session_id).await?, &sql, &sql).await
}

#[tauri::command]
fn account_drop_sql(user: String, host: String, is_role: bool) -> String {
    users::drop_sql(&user, &host, is_role)
}

#[tauri::command]
async fn account_drop(state: State<'_, AppState>, session_id: String, user: String, host: String, is_role: bool) -> AppResult<()> {
    let sql = [users::drop_sql(&user, &host, is_role)];
    users::run(&pool(&state, &session_id).await?, &sql, &sql).await
}

#[tauri::command]
async fn er_diagram(state: State<'_, AppState>, session_id: String, schema: String, tables: Vec<String>, neighbors: bool) -> AppResult<String> {
    let (columns, fks) = diagram::load(&pool(&state, &session_id).await?, &schema).await?;
    Ok(diagram::render(&columns, &fks, &tables, neighbors))
}

#[tauri::command]
fn save_diagram(path: String, text: String) -> AppResult<()> {
    diagram::save(&path, &text)
}

#[tauri::command]
async fn list_plugins(state: State<'_, AppState>, session_id: String) -> AppResult<Vec<plugins::Plugin>> {
    plugins::list(&pool(&state, &session_id).await?).await
}

#[tauri::command]
fn plugin_install_sql(name: String, library: String) -> AppResult<String> {
    plugins::install_sql(&name, &library)
}

#[tauri::command]
async fn plugin_install(state: State<'_, AppState>, session_id: String, name: String, library: String) -> AppResult<()> {
    plugins::install(&pool(&state, &session_id).await?, &name, &library).await
}

/// Progress of a running import / export, sent as the `transfer-progress` event.
#[derive(Clone, Serialize)]
struct Progress<'a> {
    id: &'a str,
    done: u64,
    total: u64,
}

/// Register a job so `cancel_job` can stop it; removed again when the job ends, whatever the outcome.
struct Job<'a> {
    state: &'a AppState,
    id: String,
    cancel: Arc<AtomicBool>,
}

impl<'a> Job<'a> {
    fn new(state: &'a AppState, id: &str, conn_id: u32) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        state.jobs.lock().unwrap().insert(id.to_string(), (cancel.clone(), conn_id));
        Job { state, id: id.to_string(), cancel }
    }
}

impl Drop for Job<'_> {
    fn drop(&mut self) {
        self.state.jobs.lock().unwrap().remove(&self.id);
    }
}

/// Export rows or DDL to `req.path` (chosen in the save dialog). Returns the number of rows written.
#[tauri::command]
async fn export_data(app: AppHandle, state: State<'_, AppState>, session_id: String, job_id: String, req: transfer::ExportReq) -> AppResult<u64> {
    let pool = pool(&state, &session_id).await?;
    let job = Job::new(&state, &job_id, 0);
    transfer::export(&pool, &req, &job.cancel, |done| {
        let _ = app.emit("transfer-progress", Progress { id: &job_id, done, total: 0 });
    })
    .await
}

/// Run an SQL file in `schema`. Returns the number of statements run.
#[tauri::command]
async fn import_sql(app: AppHandle, state: State<'_, AppState>, session_id: String, job_id: String, schema: String, path: String) -> AppResult<u64> {
    let pool = pool(&state, &session_id).await?;
    let mut conn = query::open_editor(&pool, Some(&schema)).await?;
    let job = Job::new(&state, &job_id, conn.conn_id);
    let conn = Arc::get_mut(&mut conn.conn).expect("connection not shared yet").get_mut();
    transfer::import_sql(conn, &path, &job.cancel, |done, total| {
        let _ = app.emit("transfer-progress", Progress { id: &job_id, done, total });
    })
    .await
}

#[tauri::command]
fn csv_preview(path: String, delimiter: Option<String>, header: bool) -> AppResult<transfer::CsvPreview> {
    transfer::csv_preview(&path, delimiter.as_deref(), header)
}

/// CSV file → table, in one transaction. Returns the number of rows inserted.
#[tauri::command]
async fn import_csv(app: AppHandle, state: State<'_, AppState>, session_id: String, job_id: String, req: transfer::CsvImport) -> AppResult<u64> {
    let pool = pool(&state, &session_id).await?;
    let job = Job::new(&state, &job_id, 0);
    transfer::import_csv(&pool, &req, &job.cancel, |done, total| {
        let _ = app.emit("transfer-progress", Progress { id: &job_id, done, total });
    })
    .await
}

/// Table or schema → another open session (or another schema of the same one). Returns rows copied.
#[tauri::command]
async fn copy_data(app: AppHandle, state: State<'_, AppState>, session_id: String, job_id: String, req: transfer::CopyReq) -> AppResult<u64> {
    if req.target_session == session_id && req.target_schema == req.schema {
        return Err(AppError::new("COPY_SAME", req.schema));
    }
    let (src, dst) = (pool(&state, &session_id).await?, pool(&state, &req.target_session).await?);
    let job = Job::new(&state, &job_id, 0);
    transfer::copy(&src, &dst, &req, &job.cancel, |done, total| {
        let _ = app.emit("transfer-progress", Progress { id: &job_id, done, total });
    })
    .await
}

#[tauri::command]
async fn cancel_job(state: State<'_, AppState>, session_id: String, job_id: String) -> AppResult<()> {
    let found = state.jobs.lock().unwrap().get(&job_id).cloned();
    let Some((cancel, conn_id)) = found else { return Ok(()) };
    cancel.store(true, Ordering::Relaxed);
    if conn_id > 0 {
        query::cancel(&pool(&state, &session_id).await?, conn_id).await?;
    }
    Ok(())
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
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_config_dir()?;
            app.manage(AppState { store: Store::new(dir), sessions: Mutex::new(HashMap::new()), jobs: Default::default() });
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
            close_editor,
            row_history,
            sequence_state,
            sequence_ddl,
            sequence_apply,
            sequence_setval,
            table_def,
            list_engines,
            list_collations,
            designer_sql,
            designer_apply,
            drop_sql,
            drop_object,
            er_diagram,
            save_diagram,
            list_plugins,
            plugin_install_sql,
            plugin_install,
            list_processes,
            kill_process,
            explain_process,
            list_variables,
            variable_sql,
            set_variable,
            server_status,
            list_accounts,
            account_grants,
            users_meta,
            account_sql,
            account_apply,
            privileges_sql,
            privileges_apply,
            account_drop_sql,
            account_drop,
            export_data,
            import_sql,
            csv_preview,
            import_csv,
            copy_data,
            cancel_job
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
