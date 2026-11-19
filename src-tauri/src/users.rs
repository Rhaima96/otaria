//! Users and roles: accounts (mysql.global_priv on MariaDB, mysql.user on MySQL), role membership,
//! privileges at global / database / table / column level, PROXY grants, and the SQL to change them.
//! Roles: MariaDB roles (host ''), MySQL 8 roles (accounts, `name` for `name`@`%`, else `name@host`).

use std::collections::{BTreeSet, HashSet};

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Pool};
use serde::{Deserialize, Serialize};

use crate::browse::ident;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub user: String,
    pub host: String,
    pub is_role: bool,
    pub plugin: String,
    pub locked: bool,
    /// Password marked expired (PASSWORD EXPIRE).
    pub expired: bool,
    /// Password lifetime in days: None = server default, 0 = never expires.
    pub lifetime: Option<i64>,
    pub max_questions: u64,
    pub max_updates: u64,
    pub max_connections: u64,
    pub max_user_connections: u64,
    /// Seconds, MariaDB only (0 = no limit).
    pub max_statement_time: f64,
    pub default_role: Option<String>,
    /// Roles granted to this account (MariaDB).
    pub roles: Vec<String>,
    /// Of `roles`, those granted WITH ADMIN OPTION (the account may grant them on).
    #[serde(default)]
    pub admin_roles: Vec<String>,
    /// TLS required to log in: "" (none), "SSL", "X509", or "SPECIFIED" (ISSUER / SUBJECT / CIPHER below).
    #[serde(default)]
    pub require: String,
    #[serde(default)]
    pub issuer: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub cipher: String,
    /// Accounts this one may proxy as (GRANT PROXY ON user@host), as (user, host).
    #[serde(default)]
    pub proxies: Vec<(String, String)>,
}

/// `user`@`host`, or `role` alone (MariaDB roles have no host; a MySQL role keeps its own).
fn acct(user: &str, host: &str, is_role: bool) -> String {
    if is_role && host.is_empty() { ident(user) } else { format!("{}@{}", ident(user), ident(host)) }
}

/// How a role is named in `Account::roles`: MariaDB `name`; MySQL `name` for `name`@`%`, else `name@host`.
fn role_key(user: &str, host: &str) -> String {
    if host.is_empty() || host == "%" { user.to_string() } else { format!("{user}@{host}") }
}

/// A role key in SQL. On MySQL `name@host` is split at the last `@`.
// ponytail: a MySQL role whose name holds '@' and host is '%' reads as name@host; rename it if that ever happens.
fn role_ident(key: &str, mariadb: bool) -> String {
    match key.rsplit_once('@') {
        Some((u, h)) if !mariadb => format!("{}@{}", ident(u), ident(h)),
        _ => ident(key),
    }
}

pub async fn accounts(pool: &Pool, mariadb: bool) -> AppResult<Vec<Account>> {
    let mut conn = pool.get_conn().await?;
    let sql = if mariadb {
        // global_priv.Priv is JSON; absent keys mean "default" / 0. password_last_changed = 0 means expired.
        "SELECT User, Host, IFNULL(JSON_VALUE(Priv, '$.is_role') IN ('1', 'true'), 0), IFNULL(JSON_VALUE(Priv, '$.plugin'), ''),
                IFNULL(JSON_VALUE(Priv, '$.account_locked') IN ('1', 'true'), 0), IFNULL(JSON_VALUE(Priv, '$.password_last_changed') = '0', 0),
                JSON_VALUE(Priv, '$.password_lifetime') + 0,
                IFNULL(JSON_VALUE(Priv, '$.max_questions'), 0) + 0, IFNULL(JSON_VALUE(Priv, '$.max_updates'), 0) + 0,
                IFNULL(JSON_VALUE(Priv, '$.max_connections'), 0) + 0, IFNULL(JSON_VALUE(Priv, '$.max_user_connections'), 0) + 0,
                IFNULL(JSON_VALUE(Priv, '$.max_statement_time'), 0) + 0e0, NULLIF(JSON_VALUE(Priv, '$.default_role'), ''),
                CASE JSON_VALUE(Priv, '$.ssl_type') WHEN 1 THEN 'SSL' WHEN 2 THEN 'X509' WHEN 3 THEN 'SPECIFIED' ELSE '' END,
                IFNULL(JSON_VALUE(Priv, '$.x509_issuer'), ''), IFNULL(JSON_VALUE(Priv, '$.x509_subject'), ''), IFNULL(JSON_VALUE(Priv, '$.ssl_cipher'), '')
         FROM mysql.global_priv ORDER BY 3, User, Host"
    } else {
        // A MySQL role is an account CREATE ROLE made locked, expired and without password (or one granted as a role).
        "SELECT User, Host, account_locked = 'Y' AND password_expired = 'Y' AND authentication_string = '', plugin, account_locked = 'Y',
                password_expired = 'Y', password_lifetime, max_questions, max_updates, max_connections, max_user_connections, 0e0, NULL,
                CASE ssl_type WHEN 'ANY' THEN 'SSL' WHEN 'X509' THEN 'X509' WHEN 'SPECIFIED' THEN 'SPECIFIED' ELSE '' END,
                CONVERT(x509_issuer USING utf8mb4), CONVERT(x509_subject USING utf8mb4), CONVERT(ssl_cipher USING utf8mb4)
         FROM mysql.user ORDER BY 3, User, Host"
    };
    let rows: Vec<mysql_async::Row> = conn.query(sql).await?; // 17 columns: past the tuple limit
    // (user, host, role key, admin option). MySQL < 8 has no role tables: no roles then.
    let mapping: Vec<(String, String, String, bool)> = if mariadb {
        conn.query("SELECT User, Host, Role, Admin_option = 'Y' FROM mysql.roles_mapping ORDER BY Role").await?
    } else {
        let edges: Vec<(String, String, String, String, bool)> = conn
            .query("SELECT TO_USER, TO_HOST, FROM_USER, FROM_HOST, WITH_ADMIN_OPTION = 'Y' FROM mysql.role_edges ORDER BY FROM_USER, FROM_HOST")
            .await
            .unwrap_or_default();
        edges.into_iter().map(|(u, h, ru, rh, admin)| (u, h, role_key(&ru, &rh), admin)).collect()
    };
    // MySQL: default roles (one shown; several are possible there), MariaDB has it in global_priv.
    let defaults: Vec<(String, String, String, String)> = if mariadb {
        vec![]
    } else {
        conn.query("SELECT USER, HOST, DEFAULT_ROLE_USER, DEFAULT_ROLE_HOST FROM mysql.default_roles ORDER BY 3, 4").await.unwrap_or_default()
    };
    let proxies: Vec<(String, String, String, String)> =
        conn.query("SELECT User, Host, Proxied_user, Proxied_host FROM mysql.proxies_priv ORDER BY 3, 4").await.unwrap_or_default();
    Ok(rows
        .into_iter()
        .map(|mut r| {
            let user: String = r.take(0).unwrap_or_default();
            let host: String = r.take(1).unwrap_or_default();
            let mine = || mapping.iter().filter(|(u, h, _, _)| *u == user && *h == host);
            let roles = mine().map(|m| m.2.clone()).collect();
            let admin_roles = mine().filter(|m| m.3).map(|m| m.2.clone()).collect();
            let granted_as_role = !mariadb && mapping.iter().any(|m| m.2 == role_key(&user, &host));
            let default_role: Option<String> = r.take(12).flatten();
            let default_role = default_role.or_else(|| defaults.iter().find(|d| d.0 == user && d.1 == host).map(|d| role_key(&d.2, &d.3)));
            let proxies = proxies.iter().filter(|p| p.0 == user && p.1 == host).map(|p| (p.2.clone(), p.3.clone())).collect();
            Account {
                is_role: r.take::<bool, _>(2).unwrap_or_default() || granted_as_role,
                plugin: r.take(3).unwrap_or_default(),
                locked: r.take(4).unwrap_or_default(),
                expired: r.take(5).unwrap_or_default(),
                lifetime: r.take(6).flatten(),
                max_questions: r.take(7).unwrap_or_default(),
                max_updates: r.take(8).unwrap_or_default(),
                max_connections: r.take(9).unwrap_or_default(),
                max_user_connections: r.take(10).unwrap_or_default(),
                max_statement_time: r.take(11).unwrap_or_default(),
                default_role,
                require: r.take(13).unwrap_or_default(),
                issuer: r.take::<Option<String>, _>(14).flatten().unwrap_or_default(),
                subject: r.take::<Option<String>, _>(15).flatten().unwrap_or_default(),
                cipher: r.take::<Option<String>, _>(16).flatten().unwrap_or_default(),
                proxies,
                user,
                host,
                roles,
                admin_roles,
            }
        })
        .collect())
}

/// What a grant applies to: all None = global, then database, table, column.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub schema: Option<String>,
    /// Table, or routine name when `routine` is set.
    pub table: Option<String>,
    pub column: Option<String>,
    /// PROCEDURE | FUNCTION: a routine-level grant (`ON PROCEDURE db.name`).
    #[serde(default)]
    pub routine: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub target: Target,
    /// Upper case, as in SHOW GRANTS; "GRANT OPTION" when grantable.
    pub privileges: Vec<String>,
}

/// One privilege from SHOW PRIVILEGES and the levels it can be granted at.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Privilege {
    pub name: String,
    pub global: bool,
    pub schema: bool,
    pub table: bool,
    pub column: bool,
    /// Grantable ON PROCEDURE / FUNCTION (EXECUTE, ALTER ROUTINE, SHOW CREATE ROUTINE, GRANT OPTION).
    pub routine: bool,
}

pub async fn privileges(pool: &Pool) -> AppResult<Vec<Privilege>> {
    let rows: Vec<(String, String, String)> = pool.get_conn().await?.query("SHOW PRIVILEGES").await?;
    Ok(rows
        .into_iter()
        .map(|(name, context, _)| (name.to_uppercase(), context))
        // USAGE is "no privilege"; PROXY is granted ON an account, not ON a database.
        .filter(|(n, _)| n != "USAGE" && n != "PROXY")
        .map(|(name, c)| {
            let objects = ["Databases", "Tables", "Functions", "Procedures"].iter().any(|x| c.contains(x));
            Privilege {
                // SHOW PRIVILEGES files EVENT under "Server Admin", yet it is a database privilege.
                schema: objects || name == "EVENT",
                table: c.contains("Tables"),
                column: ["SELECT", "INSERT", "UPDATE", "REFERENCES"].contains(&name.as_str()),
                routine: c.contains("Functions") || c.contains("Procedures"),
                global: true,
                name,
            }
        })
        .collect())
}

/// Privilege list of a `GRANT … ON *.* TO …` line of SHOW GRANTS (None for other lines).
fn global_line(line: &str) -> Option<(Vec<String>, bool)> {
    let rest = line.strip_prefix("GRANT ")?;
    let (list, tail) = rest.split_once(" ON *.* TO ")?;
    Some((list.split(", ").map(str::to_string).collect(), tail.contains("WITH GRANT OPTION")))
}

pub async fn grants(pool: &Pool, user: &str, host: &str, is_role: bool) -> AppResult<Vec<Grant>> {
    let mut conn = pool.get_conn().await?;
    let all: Vec<String> = privileges(pool).await?.into_iter().filter(|p| p.name != "GRANT OPTION").map(|p| p.name).collect();
    let mut out: Vec<Grant> = vec![];

    // Global: information_schema.USER_PRIVILEGES leaves out roles, so read SHOW GRANTS.
    let lines: Vec<String> = conn.query(format!("SHOW GRANTS FOR {}", acct(user, host, is_role))).await?;
    let mut global = BTreeSet::new();
    for (list, grantable) in lines.iter().filter_map(|l| global_line(l)) {
        for p in list {
            match p.as_str() {
                "USAGE" => {}
                "ALL PRIVILEGES" => global.extend(all.iter().cloned()),
                _ => {
                    global.insert(p);
                }
            }
        }
        if grantable {
            global.insert("GRANT OPTION".into());
        }
    }
    if !global.is_empty() {
        out.push(Grant { target: Target::default(), privileges: global.into_iter().collect() });
    }

    // Database, table, column: GRANTEE is 'user'@'host' ('role'@'' for a MariaDB role).
    let grantee = "CONCAT(QUOTE(?), '@', QUOTE(?))";
    let h = host;
    let mut add = |target: Target, privilege: String, grantable: bool| {
        let i = match out.iter().position(|g| g.target == target) {
            Some(i) => i,
            None => {
                out.push(Grant { target, privileges: vec![] });
                out.len() - 1
            }
        };
        out[i].privileges.push(privilege);
        if grantable && !out[i].privileges.iter().any(|p| p == "GRANT OPTION") {
            out[i].privileges.push("GRANT OPTION".into());
        }
    };
    let rows: Vec<(String, String, String)> =
        conn.exec(format!("SELECT TABLE_SCHEMA, PRIVILEGE_TYPE, IS_GRANTABLE FROM information_schema.SCHEMA_PRIVILEGES WHERE GRANTEE = {grantee} ORDER BY 1"), (user, h)).await?;
    for (s, p, g) in rows {
        add(Target { schema: Some(s), ..Default::default() }, p, g == "YES");
    }
    let rows: Vec<(String, String, String, String)> = conn
        .exec(format!("SELECT TABLE_SCHEMA, TABLE_NAME, PRIVILEGE_TYPE, IS_GRANTABLE FROM information_schema.TABLE_PRIVILEGES WHERE GRANTEE = {grantee} ORDER BY 1, 2"), (user, h))
        .await?;
    for (s, t, p, g) in rows {
        add(Target { schema: Some(s), table: Some(t), column: None, routine: None }, p, g == "YES");
    }
    let rows: Vec<(String, String, String, String)> = conn
        .exec(format!("SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, PRIVILEGE_TYPE FROM information_schema.COLUMN_PRIVILEGES WHERE GRANTEE = {grantee} ORDER BY 1, 2, 3"), (user, h))
        .await?;
    for (s, t, c, p) in rows {
        add(Target { schema: Some(s), table: Some(t), column: Some(c), routine: None }, p, false);
    }
    // Routines: no information_schema view for them in MariaDB; mysql.procs_priv (readable by whoever reads the accounts).
    let rows: Vec<(String, String, String, String)> = conn
        .exec(
            "SELECT Db, Routine_name, Routine_type, Proc_priv FROM mysql.procs_priv
              WHERE User = ? AND Host = ? AND Routine_type IN ('PROCEDURE', 'FUNCTION') ORDER BY 1, 3, 2",
            (user, h),
        )
        .await?;
    for (s, r, kind, privs) in rows {
        let target = Target { schema: Some(s), table: Some(r), column: None, routine: Some(kind) };
        for p in privs.split(',').filter(|p| !p.is_empty()) {
            let name = if p == "Grant" { "GRANT OPTION".to_string() } else { p.to_uppercase() };
            add(target.clone(), name, false);
        }
    }
    Ok(out)
}

pub async fn auth_plugins(pool: &Pool) -> AppResult<Vec<String>> {
    Ok(pool
        .get_conn()
        .await?
        .query("SELECT PLUGIN_NAME FROM information_schema.PLUGINS WHERE PLUGIN_TYPE = 'AUTHENTICATION' AND PLUGIN_STATUS = 'ACTIVE' ORDER BY 1")
        .await?)
}

/// The account editor's content. `original` = (user, host) of the account being edited, None to create.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountForm {
    pub original: Option<(String, String)>,
    #[serde(flatten)]
    pub account: Account,
    /// None = keep the current password.
    pub password: Option<String>,
    /// PASSWORD EXPIRE now (the user must change it at the next login).
    pub expire_now: bool,
}

/// Plugins that take a password; the others (unix_socket, gssapi, pam…) authenticate otherwise.
const PASSWORD_PLUGINS: [&str; 5] = ["mysql_native_password", "ed25519", "parsec", "caching_sha2_password", "sha256_password"];

/// How a password appears in the SQL: masked for the preview, or as a literal escaped for the session's sql_mode.
#[derive(Clone, Copy)]
pub enum Secret {
    Masked,
    Plain { no_backslash_escapes: bool },
}

fn lit(s: &str, secret: Secret) -> String {
    match secret {
        Secret::Masked => "'••••••'".into(),
        Secret::Plain { no_backslash_escapes } => {
            let s = if no_backslash_escapes { s.to_string() } else { s.replace('\\', "\\\\") };
            format!("'{}'", s.replace('\'', "''"))
        }
    }
}

/// A literal that is not a secret: shown as is in the preview too.
fn text(s: &str, secret: Secret) -> String {
    lit(s, if let Secret::Plain { .. } = secret { secret } else { Secret::Plain { no_backslash_escapes: false } })
}

fn check_word(s: &str) -> AppResult<()> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(AppError::new("INVALID_NAME", s.to_string()));
    }
    Ok(())
}

/// Statements turning `old` (None = new account) into `form`.
pub fn account_sql(old: Option<&Account>, form: &AccountForm, mariadb: bool, secret: Secret) -> AppResult<Vec<String>> {
    let new = &form.account;
    if new.user.is_empty() {
        return Err(AppError::new("REQUIRED", "user"));
    }
    let a = acct(&new.user, &new.host, new.is_role);
    let mut out = vec![];

    if new.is_role {
        if old.is_none() {
            out.push(format!("CREATE ROLE {a}"));
        } else if old.is_some_and(|o| o.user != new.user) {
            return Err(AppError::new("INVALID_NAME", new.user.clone())); // MariaDB cannot rename a role
        }
    } else {
        if let Some(o) = old.filter(|o| o.user != new.user || o.host != new.host) {
            out.push(format!("RENAME USER {} TO {a}", acct(&o.user, &o.host, false)));
        }
        let mut clauses = vec![];

        let plugin_changed = old.is_none_or(|o| o.plugin != new.plugin);
        if form.password.is_some() || plugin_changed {
            check_word(&new.plugin)?;
            let pw = form.password.as_deref();
            if pw.is_none() && PASSWORD_PLUGINS.contains(&new.plugin.as_str()) {
                return Err(AppError::new("REQUIRED", "password"));
            }
            clauses.push(match (mariadb, new.plugin.as_str(), pw) {
                (_, "mysql_native_password", Some(p)) => format!("IDENTIFIED BY {}", lit(p, secret)),
                (true, plugin, Some(p)) => format!("IDENTIFIED VIA {plugin} USING PASSWORD({})", lit(p, secret)),
                (true, plugin, None) => format!("IDENTIFIED VIA {plugin}"),
                (false, plugin, Some(p)) => format!("IDENTIFIED WITH {plugin} BY {}", lit(p, secret)),
                (false, plugin, None) => format!("IDENTIFIED WITH {plugin}"),
            });
        }

        let d = Account::default();
        let o = old.unwrap_or(&d);
        let specified = |a: &Account| (a.issuer.clone(), a.subject.clone(), a.cipher.clone());
        if o.require != new.require || new.require == "SPECIFIED" && specified(o) != specified(new) {
            clauses.push(match new.require.as_str() {
                "" => "REQUIRE NONE".to_string(),
                r @ ("SSL" | "X509") => format!("REQUIRE {r}"),
                "SPECIFIED" => {
                    let parts: Vec<String> = [("ISSUER", &new.issuer), ("SUBJECT", &new.subject), ("CIPHER", &new.cipher)]
                        .iter()
                        .filter(|(_, v)| !v.is_empty())
                        .map(|(k, v)| format!("{k} {}", text(v, secret)))
                        .collect();
                    if parts.is_empty() {
                        return Err(AppError::new("REQUIRED", "require"));
                    }
                    format!("REQUIRE {}", parts.join(" AND "))
                }
                r => return Err(AppError::new("INVALID_NAME", r.to_string())),
            });
        }
        let mut limits: Vec<String> = [
            ("MAX_QUERIES_PER_HOUR", o.max_questions, new.max_questions),
            ("MAX_UPDATES_PER_HOUR", o.max_updates, new.max_updates),
            ("MAX_CONNECTIONS_PER_HOUR", o.max_connections, new.max_connections),
            ("MAX_USER_CONNECTIONS", o.max_user_connections, new.max_user_connections),
        ]
        .iter()
        .filter(|(_, a, b)| a != b)
        .map(|(k, _, b)| format!("{k} {b}"))
        .collect();
        if mariadb && o.max_statement_time != new.max_statement_time {
            limits.push(format!("MAX_STATEMENT_TIME {}", new.max_statement_time.max(0.0)));
        }
        if !limits.is_empty() {
            clauses.push(format!("WITH {}", limits.join(" ")));
        }

        if old.map(|o| o.lifetime) != Some(new.lifetime) && (old.is_some() || new.lifetime.is_some()) {
            clauses.push(match new.lifetime {
                None => "PASSWORD EXPIRE DEFAULT".into(),
                Some(n) if n <= 0 => "PASSWORD EXPIRE NEVER".into(),
                Some(n) => format!("PASSWORD EXPIRE INTERVAL {n} DAY"),
            });
        }
        if o.locked != new.locked {
            clauses.push(if new.locked { "ACCOUNT LOCK".into() } else { "ACCOUNT UNLOCK".into() });
        }

        if old.is_none() {
            out.push(format!("CREATE USER {a}{}", clauses.iter().map(|c| format!(" {c}")).collect::<String>()));
        } else if !clauses.is_empty() {
            out.push(format!("ALTER USER {a} {}", clauses.join(" ")));
        }
        if form.expire_now {
            out.push(format!("ALTER USER {a} PASSWORD EXPIRE"));
        }
    }

    // Roles: membership, then the default role, which must already be granted.
    let had: HashSet<&String> = old.map(|o| o.roles.iter().collect()).unwrap_or_default();
    let has: HashSet<&String> = new.roles.iter().collect();
    let admin = |r: &String| new.admin_roles.contains(r);
    let was_admin = |r: &String| old.is_some_and(|o| o.admin_roles.contains(r));
    let ri = |r: &String| role_ident(r, mariadb);
    for r in new.roles.iter().filter(|r| !had.contains(r) || admin(r) && !was_admin(r)) {
        out.push(format!("GRANT {} TO {a}{}", ri(r), if admin(r) { " WITH ADMIN OPTION" } else { "" }));
    }
    for r in new.roles.iter().filter(|r| had.contains(r) && !admin(r) && was_admin(r)) {
        if mariadb {
            out.push(format!("REVOKE ADMIN OPTION FOR {} FROM {a}", ri(r)));
        } else {
            // MySQL has no REVOKE ADMIN OPTION: revoke the role, grant it again without it.
            out.extend([format!("REVOKE {} FROM {a}", ri(r)), format!("GRANT {} TO {a}", ri(r))]);
        }
    }
    for r in old.map(|o| o.roles.as_slice()).unwrap_or_default().iter().filter(|r| !has.contains(r)) {
        out.push(format!("REVOKE {} FROM {a}", ri(r)));
    }
    if !new.is_role && old.map(|o| &o.default_role) != Some(&new.default_role) && (old.is_some() || new.default_role.is_some()) {
        let to = if mariadb { "FOR" } else { "TO" };
        out.push(match &new.default_role {
            Some(r) => format!("SET DEFAULT ROLE {} {to} {a}", ri(r)),
            None => format!("SET DEFAULT ROLE NONE {to} {a}"),
        });
    }

    // PROXY: users only.
    if !new.is_role {
        let before = old.map(|o| o.proxies.as_slice()).unwrap_or_default();
        for (u, h) in new.proxies.iter().filter(|p| !before.contains(p)) {
            out.push(format!("GRANT PROXY ON {} TO {a}", acct(u, h, false)));
        }
        for (u, h) in before.iter().filter(|p| !new.proxies.contains(p)) {
            out.push(format!("REVOKE PROXY ON {} FROM {a}", acct(u, h, false)));
        }
    }
    Ok(out)
}

/// Privileges to add and remove on one target.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivChange {
    pub target: Target,
    pub grant: Vec<String>,
    pub revoke: Vec<String>,
}

/// GRANT / REVOKE statements. Privilege names are checked as plain words; the server rejects unknown ones.
pub fn privileges_sql(user: &str, host: &str, is_role: bool, changes: &[PrivChange]) -> AppResult<Vec<String>> {
    let a = acct(user, host, is_role);
    let mut out = vec![];
    for ch in changes {
        let t = &ch.target;
        let on = match (&t.schema, &t.table) {
            (Some(s), Some(r)) if t.routine.is_some() => {
                let kind = t.routine.as_deref().filter(|k| *k == "PROCEDURE" || *k == "FUNCTION").ok_or_else(|| AppError::new("INVALID_NAME", format!("{:?}", t.routine)))?;
                format!("{kind} {}.{}", ident(s), ident(r))
            }
            (None, _) => "*.*".to_string(),
            (Some(s), None) => format!("{}.*", ident(s)),
            (Some(s), Some(tb)) => format!("{}.{}", ident(s), ident(tb)),
        };
        let list = |privs: &[String]| -> AppResult<String> {
            for p in privs {
                if p.is_empty() || !p.chars().all(|c| c.is_ascii_uppercase() || c == ' ' || c == '_') {
                    return Err(AppError::new("INVALID_NAME", p.clone()));
                }
            }
            Ok(privs.iter().map(|p| match &t.column {
                Some(c) => format!("{p} ({})", ident(c)),
                None => p.clone(),
            }).collect::<Vec<_>>().join(", "))
        };
        if !ch.revoke.is_empty() {
            out.push(format!("REVOKE {} ON {on} FROM {a}", list(&ch.revoke)?));
        }
        if !ch.grant.is_empty() {
            // GRANT OPTION is granted with WITH GRANT OPTION (alone: on USAGE).
            let rest: Vec<String> = ch.grant.iter().filter(|p| *p != "GRANT OPTION").cloned().collect();
            let opt = rest.len() != ch.grant.len();
            let privs = if rest.is_empty() { "USAGE".to_string() } else { list(&rest)? };
            out.push(format!("GRANT {privs} ON {on} TO {a}{}", if opt { " WITH GRANT OPTION" } else { "" }));
        }
    }
    Ok(out)
}

async fn no_backslash_escapes(conn: &mut Conn) -> AppResult<bool> {
    Ok(conn.query_first::<bool, _>("SELECT @@sql_mode LIKE '%NO_BACKSLASH_ESCAPES%'").await?.unwrap_or(false))
}

pub async fn load(pool: &Pool, mariadb: bool, original: &Option<(String, String)>) -> AppResult<Option<Account>> {
    let Some((u, h)) = original else { return Ok(None) };
    let found = accounts(pool, mariadb).await?.into_iter().find(|a| a.user == *u && a.host == *h);
    found.map(Some).ok_or_else(|| AppError::new("NOT_FOUND", format!("{u}@{h}")))
}

/// Runs the statements in order (account DDL commits on its own, there is no transaction to roll back).
/// On failure the error names the statement, in its masked form.
pub async fn run(pool: &Pool, real: &[String], masked: &[String]) -> AppResult<()> {
    let mut conn = pool.get_conn().await?;
    for (i, sql) in real.iter().enumerate() {
        if let Err(e) = conn.query_drop(sql).await {
            let mut e = AppError::from(e);
            e.detail = Some(masked[i].clone());
            return Err(e);
        }
    }
    Ok(())
}

pub async fn apply_account(pool: &Pool, mariadb: bool, form: &AccountForm) -> AppResult<()> {
    let old = load(pool, mariadb, &form.original).await?;
    let nbe = no_backslash_escapes(&mut pool.get_conn().await?).await?;
    let real = account_sql(old.as_ref(), form, mariadb, Secret::Plain { no_backslash_escapes: nbe })?;
    let masked = account_sql(old.as_ref(), form, mariadb, Secret::Masked)?;
    run(pool, &real, &masked).await
}

pub fn drop_sql(user: &str, host: &str, is_role: bool) -> String {
    format!("DROP {} {}", if is_role { "ROLE" } else { "USER" }, acct(user, host, is_role))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(account: Account, original: Option<(&str, &str)>, password: Option<&str>) -> AccountForm {
        AccountForm { original: original.map(|(u, h)| (u.into(), h.into())), account, password: password.map(String::from), expire_now: false }
    }

    fn user(name: &str) -> Account {
        Account { user: name.into(), host: "%".into(), plugin: "mysql_native_password".into(), ..Default::default() }
    }

    #[test]
    fn account_sql_creates_alters_and_masks() {
        let plain = Secret::Plain { no_backslash_escapes: false };
        // Create: password escaped, options in one statement.
        let mut a = user("app");
        a.locked = true;
        a.lifetime = Some(30);
        a.max_questions = 10;
        let f = form(a.clone(), None, Some("it's\\x"));
        assert_eq!(
            account_sql(None, &f, true, plain).unwrap(),
            ["CREATE USER `app`@`%` IDENTIFIED BY 'it''s\\\\x' WITH MAX_QUERIES_PER_HOUR 10 PASSWORD EXPIRE INTERVAL 30 DAY ACCOUNT LOCK"]
        );
        assert_eq!(account_sql(None, &f, true, Secret::Masked).unwrap()[0], "CREATE USER `app`@`%` IDENTIFIED BY '••••••' WITH MAX_QUERIES_PER_HOUR 10 PASSWORD EXPIRE INTERVAL 30 DAY ACCOUNT LOCK");
        assert_eq!(lit("a\\b'", Secret::Plain { no_backslash_escapes: true }), "'a\\b'''");
        // A password plugin needs a password.
        assert_eq!(account_sql(None, &form(user("x"), None, None), true, plain).unwrap_err().code, "REQUIRED");

        // Edit: rename, other plugin, unlock, never expires, roles, default role.
        let mut old = a.clone();
        old.roles = vec!["r1".into()];
        let mut new = a.clone();
        new.user = "app2".into();
        new.host = "10.0.%".into();
        new.plugin = "ed25519".into();
        new.locked = false;
        new.lifetime = Some(0);
        new.max_statement_time = 2.5;
        new.roles = vec!["r2".into()];
        new.default_role = Some("r2".into());
        let mut f = form(new, Some(("app", "%")), Some("pw"));
        f.expire_now = true;
        assert_eq!(
            account_sql(Some(&old), &f, true, Secret::Masked).unwrap(),
            [
                "RENAME USER `app`@`%` TO `app2`@`10.0.%`",
                "ALTER USER `app2`@`10.0.%` IDENTIFIED VIA ed25519 USING PASSWORD('••••••') WITH MAX_STATEMENT_TIME 2.5 PASSWORD EXPIRE NEVER ACCOUNT UNLOCK",
                "ALTER USER `app2`@`10.0.%` PASSWORD EXPIRE",
                "GRANT `r2` TO `app2`@`10.0.%`",
                "REVOKE `r1` FROM `app2`@`10.0.%`",
                "SET DEFAULT ROLE `r2` FOR `app2`@`10.0.%`",
            ]
        );
        // Nothing changed: nothing to run. MySQL syntax for plugins.
        assert!(account_sql(Some(&old), &form(old.clone(), Some(("app", "%")), None), true, plain).unwrap().is_empty());
        let mut m = user("m");
        m.plugin = "caching_sha2_password".into();
        assert_eq!(account_sql(None, &form(m, None, Some("p")), false, plain).unwrap(), ["CREATE USER `m`@`%` IDENTIFIED WITH caching_sha2_password BY 'p'"]);
        // Roles: CREATE ROLE, no host; bad plugin names refused.
        let role = Account { user: "readers".into(), is_role: true, ..Default::default() };
        assert_eq!(account_sql(None, &form(role, None, None), true, plain).unwrap(), ["CREATE ROLE `readers`"]);
        let mut bad = user("b");
        bad.plugin = "x; DROP".into();
        assert_eq!(account_sql(None, &form(bad, None, Some("p")), true, plain).unwrap_err().code, "INVALID_NAME");
        assert_eq!(drop_sql("readers", "", true), "DROP ROLE `readers`");
    }

    #[test]
    fn require_admin_option_and_routine_grants() {
        let plain = Secret::Plain { no_backslash_escapes: false };
        let mut old = user("u");
        old.roles = vec!["r1".into(), "r2".into()];
        old.admin_roles = vec!["r2".into()];
        let mut new = old.clone();
        new.require = "X509".into();
        new.admin_roles = vec!["r1".into()];
        new.roles.push("r3".into());
        new.admin_roles.push("r3".into());
        let f = AccountForm { original: Some(("u".into(), "%".into())), account: new.clone(), password: None, expire_now: false };
        assert_eq!(
            account_sql(Some(&old), &f, true, plain).unwrap(),
            [
                "ALTER USER `u`@`%` REQUIRE X509",
                "GRANT `r1` TO `u`@`%` WITH ADMIN OPTION",
                "GRANT `r3` TO `u`@`%` WITH ADMIN OPTION",
                "REVOKE ADMIN OPTION FOR `r2` FROM `u`@`%`",
            ]
        );
        // SPECIFIED: the non-empty conditions, joined by AND; shown as is in the preview; none at all is refused.
        let mut spec = old.clone();
        spec.require = "SPECIFIED".into();
        assert_eq!(account_sql(Some(&old), &AccountForm { account: spec.clone(), ..f.clone() }, true, plain).unwrap_err().code, "REQUIRED");
        spec.issuer = "/CN=it's CA".into();
        spec.cipher = "AES256-SHA".into();
        let want = ["ALTER USER `u`@`%` REQUIRE ISSUER '/CN=it''s CA' AND CIPHER 'AES256-SHA'"];
        assert_eq!(account_sql(Some(&old), &AccountForm { account: spec.clone(), ..f.clone() }, true, plain).unwrap(), want);
        assert_eq!(account_sql(Some(&old), &AccountForm { account: spec.clone(), ..f.clone() }, true, Secret::Masked).unwrap(), want);
        assert!(account_sql(Some(&spec), &AccountForm { account: spec.clone(), ..f.clone() }, true, plain).unwrap().is_empty(), "unchanged");
        let mut none = new.clone();
        none.require = String::new();
        assert_eq!(account_sql(Some(&new), &AccountForm { account: none, ..f.clone() }, true, plain).unwrap(), ["ALTER USER `u`@`%` REQUIRE NONE"]);
        let mut bad = new.clone();
        bad.require = "SSL; DROP".into();
        assert_eq!(account_sql(Some(&new), &AccountForm { account: bad, ..f }, true, plain).unwrap_err().code, "INVALID_NAME");

        let r = |kind: &str| Target { schema: Some("db".into()), table: Some("p".into()), column: None, routine: Some(kind.into()) };
        let ch = PrivChange { target: r("PROCEDURE"), grant: vec!["EXECUTE".into(), "GRANT OPTION".into()], revoke: vec!["ALTER ROUTINE".into()] };
        assert_eq!(
            privileges_sql("u", "%", false, &[ch]).unwrap(),
            ["REVOKE ALTER ROUTINE ON PROCEDURE `db`.`p` FROM `u`@`%`", "GRANT EXECUTE ON PROCEDURE `db`.`p` TO `u`@`%` WITH GRANT OPTION"]
        );
        let bad = PrivChange { target: r("TABLE"), grant: vec!["EXECUTE".into()], revoke: vec![] };
        assert_eq!(privileges_sql("u", "%", false, &[bad]).unwrap_err().code, "INVALID_NAME");
    }

    #[test]
    fn mysql_roles_and_proxies() {
        let plain = Secret::Plain { no_backslash_escapes: false };
        // MySQL roles: `r` is `r`@`%`, others keep their host; TO for the default role; no REVOKE ADMIN OPTION.
        assert_eq!((role_key("r", "%"), role_key("r", "10.%"), role_key("r", "")), ("r".into(), "r@10.%".into(), "r".into()));
        assert_eq!((role_ident("r@10.%", false), role_ident("a@b", true)), ("`r`@`10.%`".into(), "`a@b`".into()));
        let mut old = user("u");
        old.roles = vec!["r1".into(), "r2@10.%".into()];
        old.admin_roles = vec!["r1".into()];
        let mut new = old.clone();
        new.admin_roles = vec![];
        new.roles = vec!["r1".into(), "r3".into()];
        new.default_role = Some("r3".into());
        let f = AccountForm { original: Some(("u".into(), "%".into())), account: new.clone(), password: None, expire_now: false };
        assert_eq!(
            account_sql(Some(&old), &f, false, plain).unwrap(),
            ["GRANT `r3` TO `u`@`%`", "REVOKE `r1` FROM `u`@`%`", "GRANT `r1` TO `u`@`%`", "REVOKE `r2`@`10.%` FROM `u`@`%`", "SET DEFAULT ROLE `r3` TO `u`@`%`"]
        );
        // A MySQL role keeps its host; a MariaDB role has none.
        let role = |host: &str| Account { user: "rr".into(), host: host.into(), is_role: true, ..Default::default() };
        let create = |a: Account, mariadb| account_sql(None, &AccountForm { original: None, account: a, password: None, expire_now: false }, mariadb, plain).unwrap();
        assert_eq!((create(role("%"), false), create(role(""), true)), (vec!["CREATE ROLE `rr`@`%`".to_string()], vec!["CREATE ROLE `rr`".to_string()]));
        assert_eq!(drop_sql("rr", "%", true), "DROP ROLE `rr`@`%`");

        // PROXY: added and removed by difference, users only.
        let mut p_old = user("u");
        p_old.proxies = vec![("a".into(), "b".into()), ("".into(), "%".into())];
        let mut p_new = p_old.clone();
        p_new.proxies = vec![("".into(), "%".into()), ("c'd".into(), "localhost".into())];
        let f = AccountForm { account: p_new, ..f };
        assert_eq!(
            account_sql(Some(&p_old), &f, true, plain).unwrap(),
            ["GRANT PROXY ON `c'd`@`localhost` TO `u`@`%`", "REVOKE PROXY ON `a`@`b` FROM `u`@`%`"]
        );
    }

    #[test]
    fn privileges_sql_by_level() {
        let t = |s: Option<&str>, tb: Option<&str>, c: Option<&str>| Target { schema: s.map(String::from), table: tb.map(String::from), column: c.map(String::from), routine: None };
        let ch = |target, grant: &[&str], revoke: &[&str]| PrivChange { target, grant: grant.iter().map(|s| s.to_string()).collect(), revoke: revoke.iter().map(|s| s.to_string()).collect() };
        assert_eq!(
            privileges_sql(
                "u",
                "%",
                false,
                &[
                    ch(t(None, None, None), &["PROCESS", "READ_ONLY ADMIN"], &["SUPER"]),
                    ch(t(Some("db"), None, None), &["SELECT", "GRANT OPTION"], &[]),
                    ch(t(Some("db"), Some("t`x"), None), &[], &["DELETE", "GRANT OPTION"]),
                    ch(t(Some("db"), Some("t"), Some("id")), &["SELECT", "UPDATE"], &[]),
                    ch(t(Some("db"), Some("t"), None), &["GRANT OPTION"], &[]),
                ]
            )
            .unwrap(),
            [
                "REVOKE SUPER ON *.* FROM `u`@`%`",
                "GRANT PROCESS, READ_ONLY ADMIN ON *.* TO `u`@`%`",
                "GRANT SELECT ON `db`.* TO `u`@`%` WITH GRANT OPTION",
                "REVOKE DELETE, GRANT OPTION ON `db`.`t``x` FROM `u`@`%`",
                "GRANT SELECT (`id`), UPDATE (`id`) ON `db`.`t` TO `u`@`%`",
                "GRANT USAGE ON `db`.`t` TO `u`@`%` WITH GRANT OPTION",
            ]
        );
        assert_eq!(privileges_sql("r", "", true, &[ch(t(None, None, None), &["SELECT"], &[])]).unwrap(), ["GRANT SELECT ON *.* TO `r`"]);
        assert_eq!(privileges_sql("u", "%", false, &[ch(t(None, None, None), &["SELECT; DROP"], &[])]).unwrap_err().code, "INVALID_NAME");
        assert_eq!(global_line("GRANT SELECT, PROCESS ON *.* TO `r` WITH GRANT OPTION"), Some((vec!["SELECT".into(), "PROCESS".into()], true)));
        assert_eq!(global_line("GRANT SELECT ON `db`.* TO `r`"), None);
        assert_eq!(global_line("GRANT `r` TO `u`@`%`"), None);
    }

    /// OTARIA_TEST_DB="host:port:user:password" cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn users_on_test_server() {
        let spec = std::env::var("OTARIA_TEST_DB").expect("set OTARIA_TEST_DB");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let pool = Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        );
        let mut conn = pool.get_conn().await.unwrap();
        conn.query_drop("DROP USER IF EXISTS 'ot_u'@'%', 'ot_u2'@'localhost'").await.unwrap();
        conn.query_drop("DROP ROLE IF EXISTS ot_r").await.unwrap();
        conn.query_drop("CREATE DATABASE IF NOT EXISTS ot_users").await.unwrap();
        conn.query_drop("CREATE TABLE IF NOT EXISTS ot_users.t (id INT PRIMARY KEY, v INT)").await.unwrap();

        // Role, then a user with options, a quote in the password, and the role as default.
        let role = Account { user: "ot_r".into(), is_role: true, ..Default::default() };
        apply_account(&pool, true, &form(role, None, None)).await.unwrap();
        let mut u = user("ot_u");
        u.locked = true;
        u.lifetime = Some(30);
        u.max_user_connections = 3;
        u.max_statement_time = 5.0;
        u.roles = vec!["ot_r".into()];
        u.default_role = Some("ot_r".into());
        apply_account(&pool, true, &form(u, None, Some("p'w\\1"))).await.unwrap();

        let all = accounts(&pool, true).await.unwrap();
        let got = all.iter().find(|a| a.user == "ot_u").unwrap();
        assert!(got.locked && !got.is_role && !got.expired, "{got:?}");
        assert_eq!((got.lifetime, got.max_user_connections, got.max_statement_time), (Some(30), 3, 5.0));
        assert_eq!((got.roles.clone(), got.default_role.clone()), (vec!["ot_r".to_string()], Some("ot_r".to_string())));
        assert!(all.iter().any(|a| a.user == "ot_r" && a.is_role));

        // The password with a quote and a backslash works for logging in once unlocked.
        let mut edit = got.clone();
        edit.locked = false;
        apply_account(&pool, true, &form(edit, Some(("ot_u", "%")), None)).await.unwrap();
        let login = Pool::new(mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some("ot_u")).pass(Some("p'w\\1")).prefer_socket(false));
        login.get_conn().await.expect("login with the new password");
        login.disconnect().await.unwrap();

        // Privileges at every level, for the user and the role.
        let t = |s: Option<&str>, tb: Option<&str>, c: Option<&str>| Target { schema: s.map(String::from), table: tb.map(String::from), column: c.map(String::from), routine: None };
        let ch = |target, grant: &[&str]| PrivChange { target, grant: grant.iter().map(|s| s.to_string()).collect(), revoke: vec![] };
        let sql = privileges_sql(
            "ot_u",
            "%",
            false,
            &[
                ch(t(None, None, None), &["PROCESS"]),
                ch(t(Some("ot_users"), None, None), &["SELECT", "GRANT OPTION"]),
                ch(t(Some("ot_users"), Some("t"), None), &["DELETE"]),
                ch(t(Some("ot_users"), Some("t"), Some("v")), &["UPDATE"]),
            ],
        )
        .unwrap();
        run(&pool, &sql, &sql).await.unwrap();
        let sql = privileges_sql("ot_r", "", true, &[ch(t(None, None, None), &["SELECT", "SLAVE MONITOR"])]).unwrap();
        run(&pool, &sql, &sql).await.unwrap();

        let g = grants(&pool, "ot_u", "%", false).await.unwrap();
        let find = |target: Target| g.iter().find(|x| x.target == target).map(|x| x.privileges.clone()).unwrap_or_default();
        assert_eq!(find(t(None, None, None)), ["PROCESS"]);
        assert_eq!(find(t(Some("ot_users"), None, None)), ["SELECT", "GRANT OPTION"]);
        assert_eq!(find(t(Some("ot_users"), Some("t"), None)), ["DELETE"]);
        assert_eq!(find(t(Some("ot_users"), Some("t"), Some("v"))), ["UPDATE"]);
        let rg = grants(&pool, "ot_r", "", true).await.unwrap();
        assert_eq!(rg[0].privileges, ["SELECT", "SLAVE MONITOR"], "role global privileges from SHOW GRANTS");

        // Routine grants, REQUIRE SSL and a role WITH ADMIN OPTION, read back.
        conn.query_drop("CREATE PROCEDURE ot_users.pr() SELECT 1").await.unwrap();
        let pr = Target { schema: Some("ot_users".into()), table: Some("pr".into()), column: None, routine: Some("PROCEDURE".into()) };
        let sql = privileges_sql("ot_u", "%", false, &[ch(pr.clone(), &["EXECUTE", "ALTER ROUTINE", "GRANT OPTION"])]).unwrap();
        run(&pool, &sql, &sql).await.unwrap();
        let g = grants(&pool, "ot_u", "%", false).await.unwrap();
        let mut got_pr = g.iter().find(|x| x.target == pr).map(|x| x.privileges.clone()).unwrap_or_default();
        got_pr.sort();
        assert_eq!(got_pr, ["ALTER ROUTINE", "EXECUTE", "GRANT OPTION"]);
        assert!(privileges(&pool).await.unwrap().iter().any(|p| p.name == "EXECUTE" && p.routine));
        let mut sec = accounts(&pool, true).await.unwrap().into_iter().find(|a| a.user == "ot_u").unwrap();
        sec.require = "SSL".into();
        sec.admin_roles = vec!["ot_r".into()];
        apply_account(&pool, true, &form(sec, Some(("ot_u", "%")), None)).await.unwrap();
        let sec = accounts(&pool, true).await.unwrap().into_iter().find(|a| a.user == "ot_u").unwrap();
        assert_eq!((sec.require.as_str(), sec.admin_roles.clone()), ("SSL", vec!["ot_r".to_string()]));

        // Revoke, rename; a failing statement reports itself.
        let rv = PrivChange { target: t(Some("ot_users"), None, None), grant: vec![], revoke: vec!["SELECT".into(), "GRANT OPTION".into()] };
        let sql = privileges_sql("ot_u", "%", false, &[rv]).unwrap();
        run(&pool, &sql, &sql).await.unwrap();
        assert!(grants(&pool, "ot_u", "%", false).await.unwrap().iter().all(|x| x.target != t(Some("ot_users"), None, None)));
        let mut renamed = accounts(&pool, true).await.unwrap().into_iter().find(|a| a.user == "ot_u").unwrap();
        renamed.user = "ot_u2".into();
        renamed.host = "localhost".into();
        apply_account(&pool, true, &form(renamed, Some(("ot_u", "%")), None)).await.unwrap();
        let bad = ["GRANT NO_SUCH_PRIV ON *.* TO `ot_u2`@`localhost`".to_string()];
        assert_eq!(run(&pool, &bad, &bad).await.unwrap_err().detail.as_deref(), Some(bad[0].as_str()));
        assert!(privileges(&pool).await.unwrap().iter().any(|p| p.name == "EVENT" && p.schema && !p.table));

        // REQUIRE ISSUER / CIPHER and PROXY, written then read back.
        let mut sp = accounts(&pool, true).await.unwrap().into_iter().find(|a| a.user == "ot_u2").unwrap();
        sp.require = "SPECIFIED".into();
        sp.issuer = "/CN=it's CA".into();
        sp.cipher = "AES256-SHA".into();
        sp.proxies = vec![("ot_p".into(), "%".into())];
        apply_account(&pool, true, &form(sp, Some(("ot_u2", "localhost")), None)).await.unwrap();
        let back = accounts(&pool, true).await.unwrap().into_iter().find(|a| a.user == "ot_u2").unwrap();
        assert_eq!((back.require.as_str(), back.issuer.as_str(), back.subject.as_str(), back.cipher.as_str()), ("SPECIFIED", "/CN=it's CA", "", "AES256-SHA"));
        assert_eq!(back.proxies, [("ot_p".to_string(), "%".to_string())]);
        let mut none = back.clone();
        none.proxies.clear();
        apply_account(&pool, true, &form(none, Some(("ot_u2", "localhost")), None)).await.unwrap();
        assert!(accounts(&pool, true).await.unwrap().into_iter().find(|a| a.user == "ot_u2").unwrap().proxies.is_empty());

        conn.query_drop(drop_sql("ot_u2", "localhost", false)).await.unwrap();
        conn.query_drop(drop_sql("ot_r", "", true)).await.unwrap();
        conn.query_drop("DROP DATABASE ot_users").await.unwrap();
    }

    /// MySQL 8 roles: OTARIA_TEST_MYSQL="host:port:user:password" cargo test users_on_mysql -- --ignored
    #[tokio::test]
    #[ignore]
    async fn users_on_mysql() {
        let spec = std::env::var("OTARIA_TEST_MYSQL").expect("set OTARIA_TEST_MYSQL");
        let p: Vec<&str> = spec.splitn(4, ':').collect();
        let pool = Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname(p[0]).tcp_port(p[1].parse().unwrap()).user(Some(p[2])).pass(Some(p[3])).prefer_socket(false),
        );
        let mut conn = pool.get_conn().await.unwrap();
        conn.query_drop("DROP USER IF EXISTS ot_mu@'%'; DROP ROLE IF EXISTS ot_mr, ot_mr2@'10.%'").await.unwrap();
        let new = |user: &str, host: &str, is_role: bool| AccountForm {
            original: None,
            account: Account { user: user.into(), host: host.into(), is_role, plugin: "caching_sha2_password".into(), ..Default::default() },
            password: Some("pw-Ot1".into()),
            expire_now: false,
        };
        apply_account(&pool, false, &new("ot_mr", "%", true)).await.unwrap();
        apply_account(&pool, false, &new("ot_mr2", "10.%", true)).await.unwrap();
        apply_account(&pool, false, &new("ot_mu", "%", false)).await.unwrap();
        let find = |all: &[Account], u: &str| all.iter().find(|a| a.user == u).cloned().unwrap();
        let all = accounts(&pool, false).await.unwrap();
        assert!(find(&all, "ot_mr").is_role && find(&all, "ot_mr2").is_role && !find(&all, "ot_mu").is_role);

        // Both roles granted (one WITH ADMIN OPTION), one as default.
        let mut u = find(&all, "ot_mu");
        u.roles = vec!["ot_mr".into(), "ot_mr2@10.%".into()];
        u.admin_roles = vec!["ot_mr".into()];
        u.default_role = Some("ot_mr2@10.%".into());
        apply_account(&pool, false, &AccountForm { original: Some(("ot_mu".into(), "%".into())), account: u, password: None, expire_now: false }).await.unwrap();
        let u = find(&accounts(&pool, false).await.unwrap(), "ot_mu");
        let mut roles = u.roles.clone();
        roles.sort();
        assert_eq!((roles, u.admin_roles.clone(), u.default_role.clone()), (vec!["ot_mr".to_string(), "ot_mr2@10.%".to_string()], vec!["ot_mr".to_string()], Some("ot_mr2@10.%".to_string())));

        // Admin option dropped (revoke + grant), one role revoked, default role none.
        let mut v = u.clone();
        v.admin_roles.clear();
        v.roles = vec!["ot_mr".into()];
        v.default_role = None;
        apply_account(&pool, false, &AccountForm { original: Some(("ot_mu".into(), "%".into())), account: v, password: None, expire_now: false }).await.unwrap();
        let v = find(&accounts(&pool, false).await.unwrap(), "ot_mu");
        assert_eq!((v.roles.clone(), v.admin_roles.clone(), v.default_role.clone()), (vec!["ot_mr".to_string()], vec![], None));

        // A role's own privileges are read under its host.
        let sql = privileges_sql("ot_mr", "%", true, &[PrivChange { target: Target::default(), grant: vec!["PROCESS".into()], revoke: vec![] }]).unwrap();
        run(&pool, &sql, &sql).await.unwrap();
        assert_eq!(grants(&pool, "ot_mr", "%", true).await.unwrap()[0].privileges, ["PROCESS"]);

        conn.query_drop(format!("{}; {}; {}", drop_sql("ot_mu", "%", false), drop_sql("ot_mr", "%", true), drop_sql("ot_mr2", "10.%", true))).await.unwrap();
    }
}
