use crate::{
    auth,
    config::{Config, private_dir, private_write},
    error::{ApiError, ApiResult},
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
pub struct AppState {
    pub elicitations: crate::elicitation::Elicitations,
    pub permissions: crate::permissions::Permissions,
    pub home: PathBuf,
    pub config: Config,
    pub db: Mutex<Connection>,
    pub login_gate: Arc<Semaphore>,
    pub operation_gate: Arc<Semaphore>,
    pub progress: Mutex<HashMap<String, Value>>,
    pub cancellations: Mutex<HashMap<String, (String, tokio::sync::watch::Sender<bool>)>>,
    pub active_sessions: Mutex<HashSet<String>>,
    pub provision_gate: Mutex<()>,
    pub acp_clients: Mutex<HashMap<String, Arc<tokio::sync::Mutex<crate::acp::LiveSession>>>>,
}
impl AppState {
    pub fn open(home: PathBuf) -> Result<Arc<Self>> {
        private_dir(&home)?;
        private_dir(&home.join("secrets"))?;
        private_dir(&home.join("jobs"))?;
        let config = Config::load(&home)?;
        let db = Connection::open(home.join("state.sqlite3"))?;
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA foreign_keys=ON;PRAGMA busy_timeout=5000;
CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL,id TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(kind,id));
CREATE TABLE IF NOT EXISTS tokens(hash TEXT PRIMARY KEY,id TEXT NOT NULL,scopes TEXT NOT NULL,expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS browser_sessions(hash TEXT PRIMARY KEY,owner TEXT NOT NULL,csrf TEXT NOT NULL,expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS login_attempts(key TEXT PRIMARY KEY,failures INTEGER NOT NULL,locked_until INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS views(id TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS audit(seq INTEGER PRIMARY KEY AUTOINCREMENT,at INTEGER NOT NULL,kind TEXT NOT NULL,resource_id TEXT,data TEXT NOT NULL);")?;
        db.execute("UPDATE records SET value=json_set(value,'$.status','interrupted','$.note','The service restarted. Inspect the remote result before retrying.') WHERE kind='jobs' AND json_extract(value,'$.status') IN ('queued','running')", [])?;
        db.execute("UPDATE records SET value=json_set(value,'$.status','interrupted','$.error','The service restarted during this turn; inspect the agent before retrying.') WHERE kind='sessions' AND json_extract(value,'$.status')='running'", [])?;
        let path = home.join("secrets/control-token");
        let token = if path.exists() {
            std::fs::read_to_string(&path)?.trim().to_owned()
        } else {
            let token = auth::random_token();
            private_write(&path, token.as_bytes())?;
            token
        };
        anyhow::ensure!(token.len() >= 32, "Invalid control credential");
        db.execute("DELETE FROM tokens WHERE id='local-control'", [])?;
        db.execute(
            "INSERT INTO tokens VALUES(?1,'local-control','[\"admin\"]',?2)",
            params![
                auth::digest(&token),
                now_ms() + 100_i64 * 366 * 86400 * 1000
            ],
        )?;
        Ok(Arc::new(Self {
            permissions: crate::permissions::Permissions::default(),
            elicitations: crate::elicitation::Elicitations::default(),
            home,
            config,
            db: Mutex::new(db),
            login_gate: Arc::new(Semaphore::new(2)),
            operation_gate: Arc::new(Semaphore::new(4)),
            progress: Mutex::new(HashMap::new()),
            cancellations: Mutex::new(HashMap::new()),
            active_sessions: Mutex::new(HashSet::new()),
            provision_gate: Mutex::new(()),
            acp_clients: Mutex::new(HashMap::new()),
        }))
    }
    pub fn audit(&self, kind: &str, resource: Option<&str>, data: Value) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO audit(at,kind,resource_id,data) VALUES(?1,?2,?3,?4)",
            params![now_ms(), kind, resource, data.to_string()],
        )?;
        db.execute(
            "DELETE FROM audit WHERE seq < (SELECT COALESCE(MAX(seq),0)-5000 FROM audit)",
            [],
        )?;
        Ok(())
    }
    pub fn list(&self, kind: &str) -> ApiResult<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut statement = db.prepare("SELECT value FROM records WHERE kind=?1 ORDER BY json_extract(value,'$.updated_at') DESC")?;
        let values = statement
            .query_map([kind], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(values
            .iter()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect())
    }
    pub fn read(&self, kind: &str, id: &str) -> ApiResult<Value> {
        let raw: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM records WHERE kind=?1 AND id=?2",
                params![kind, id],
                |r| r.get(0),
            )
            .optional()?;
        serde_json::from_str(&raw.ok_or_else(ApiError::missing)?)
            .map_err(|_| ApiError::bad("Invalid stored record"))
    }
    pub fn save(&self, kind: &str, mut value: Value, expected: Option<u64>) -> ApiResult<Value> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let id = value["id"]
            .as_str()
            .ok_or_else(|| ApiError::bad("Record ID required"))?
            .to_owned();
        let old: Option<String> = tx
            .query_row(
                "SELECT value FROM records WHERE kind=?1 AND id=?2",
                params![kind, id],
                |r| r.get(0),
            )
            .optional()?;
        let old: Option<Value> = old.and_then(|s| serde_json::from_str(&s).ok());
        let revision = old
            .as_ref()
            .and_then(|v| v["revision"].as_u64())
            .unwrap_or(0);
        if let Some(e) = expected
            && e != revision
        {
            return Err(ApiError(
                axum::http::StatusCode::CONFLICT,
                "This record changed; refresh before saving".into(),
            ));
        }
        value["revision"] = json!(revision + 1);
        value["created_at"] = old
            .as_ref()
            .map(|v| v["created_at"].clone())
            .unwrap_or(json!(now_ms()));
        value["updated_at"] = json!(now_ms());
        tx.execute("INSERT INTO records VALUES(?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET value=excluded.value",params![kind,id,value.to_string()])?;
        tx.commit()?;
        Ok(value)
    }
    pub fn remove(&self, kind: &str, id: &str, expected: u64) -> ApiResult<()> {
        let affected = self.db.lock().unwrap().execute(
            "DELETE FROM records WHERE kind=?1 AND id=?2 AND json_extract(value,'$.revision')=?3",
            params![kind, id, expected as i64],
        )?;
        if affected == 0 {
            return Err(ApiError(
                axum::http::StatusCode::CONFLICT,
                "The record changed or was already removed".into(),
            ));
        }
        Ok(())
    }
    pub fn secret(&self, name: &str) -> ApiResult<String> {
        if !crate::config::safe_name(name) || name == "control-token" {
            return Err(ApiError::bad("Invalid credential reference"));
        }
        Ok(
            std::fs::read_to_string(self.home.join("secrets").join(name))?
                .trim()
                .to_owned(),
        )
    }
}
