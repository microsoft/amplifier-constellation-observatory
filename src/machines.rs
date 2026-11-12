//! Optional, observation-only machine pairing. Agent access and execution stay separate.
use crate::{
    auth,
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rand::Rng;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{Cursor, Write},
    path::PathBuf,
    sync::Arc,
};

const TTL: i64 = 10 * 60 * 1000;
const FRESH: i64 = 45 * 1000;
const MAX_PENDING: i64 = 12;

pub fn schema(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS machine_pairings(id TEXT PRIMARY KEY,actor TEXT NOT NULL,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS machine_grants(host_id TEXT PRIMARY KEY,machine_id TEXT NOT NULL UNIQUE,credential_hash TEXT NOT NULL UNIQUE,revoked_at INTEGER,paired_at INTEGER NOT NULL,pairing_id TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS machine_observations(host_id TEXT PRIMARY KEY,value TEXT NOT NULL);")
}
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/machine-pairings", get(list).post(create))
        .route("/api/v1/machine-pairings/{id}", get(read))
        .route("/api/v1/machine-pairings/{id}/cancel", post(cancel))
        .route("/api/v1/machine-pairings/{id}/redeem", post(redeem))
        .route("/api/v1/machines/{id}/heartbeat", post(heartbeat))
        .route("/api/v1/machines/{id}/revoke", post(revoke))
        .route(
            "/api/v1/machine-pairings/{id}/setup/{platform}",
            get(download),
        )
}
/// Only these existing machine-scoped operations are exposed by the optional network listener.
pub(crate) fn helper_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/machine-pairings/{id}/redeem", post(redeem))
        .route("/api/v1/machines/{id}/heartbeat", post(heartbeat))
        .route("/api/v1/machines/{id}/revoke", post(revoke_self))
}
fn conflict(message: &str) -> ApiError {
    ApiError(StatusCode::CONFLICT, message.into())
}
fn uuid(value: &str) -> ApiResult<()> {
    if uuid::Uuid::parse_str(value).is_err() {
        return Err(ApiError::bad("Use a valid request identity"));
    }
    Ok(())
}
fn decode(value: String) -> ApiResult<Value> {
    serde_json::from_str(&value).map_err(|_| ApiError::bad("Invalid machine record"))
}
fn pairing(db: &Connection, id: &str) -> ApiResult<(String, Value)> {
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT actor,value FROM machine_pairings WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (actor, value) = row.ok_or_else(ApiError::missing)?;
    Ok((actor, decode(value)?))
}
fn save_pairing(db: &Connection, id: &str, value: &Value) -> ApiResult<()> {
    db.execute(
        "UPDATE machine_pairings SET value=?2 WHERE id=?1",
        params![id, value.to_string()],
    )?;
    Ok(())
}
fn visible(mut p: Value) -> Value {
    if p["state"] == "waiting" && p["expires_at"].as_i64().unwrap_or(0) <= now_ms() {
        p["state"] = json!("expired");
    }
    for key in ["claim_hash", "claim_request_id", "machine_id", "failures"] {
        p.as_object_mut().unwrap().remove(key);
    }
    if p["state"] != "waiting" {
        p["code"] = Value::Null;
    }
    p
}
fn authorized_pairing(s: &AppState, h: &HeaderMap, id: &str, mutation: bool) -> ApiResult<Value> {
    let actor = auth::check(s, h, "provision", mutation)?.actor;
    let (owner, p) = pairing(&s.db.lock().unwrap(), id)?;
    if actor != owner {
        return Err(ApiError::forbidden());
    }
    Ok(p)
}
fn envelope(s: &AppState, p: Value) -> ApiResult<Value> {
    let downloads = artifacts(s)?
        .into_iter()
        .map(|a| a.descriptor(p["id"].as_str()))
        .collect::<Vec<_>>();
    Ok(json!({"pairing":visible(p),"downloads":downloads}))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    request_id: String,
}
async fn create(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<Create>,
) -> ApiResult<Json<Value>> {
    let actor = auth::check(&s, &h, "provision", true)?.actor;
    uuid(&body.request_id)?;
    crate::machine_network::endpoint(&s).await?;
    let p = {
        let mut db = s.db.lock().unwrap();
        let tx = db.transaction()?;
        if let Ok((owner, p)) = pairing(&tx, &body.request_id) {
            if owner != actor {
                return Err(ApiError::forbidden());
            }
            p
        } else {
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM machine_pairings WHERE json_extract(value,'$.state')='waiting' AND json_extract(value,'$.expires_at')>?1",[now_ms()],|r|r.get(0))?;
            if count >= MAX_PENDING {
                return Err(ApiError(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Finish or cancel an existing pairing first".into(),
                ));
            }
            // Keep request identities: an old create retry must never mint another code.
            tx.execute("UPDATE machine_pairings SET value=json_set(value,'$.state','expired','$.code',NULL) WHERE json_extract(value,'$.state')='waiting' AND json_extract(value,'$.expires_at')<=?1",[now_ms()])?;
            let alphabet = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
            let mut rng = rand::rngs::OsRng;
            let raw: String = (0..8)
                .map(|_| alphabet[rng.gen_range(0..alphabet.len())] as char)
                .collect();
            let p = json!({"id":body.request_id,"request_id":body.request_id,"code":format!("{}-{}",&raw[..4],&raw[4..]),"state":"waiting","expires_at":now_ms()+TTL,"created_at":now_ms(),"host_id":null,"failures":0});
            tx.execute(
                "INSERT INTO machine_pairings VALUES(?1,?2,?3)",
                params![body.request_id, actor, p.to_string()],
            )?;
            tx.commit()?;
            p
        }
    };
    Ok(Json(envelope(&s, p)?))
}
async fn list(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let actor = auth::check(&s, &h, "provision", false)?.actor;
    let values = {
        let db = s.db.lock().unwrap();
        let mut q=db.prepare("SELECT value FROM machine_pairings WHERE actor=?1 ORDER BY json_extract(value,'$.created_at') DESC LIMIT 30")?;
        q.query_map([actor], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let rows = values
        .into_iter()
        .map(decode)
        .map(|v| v.map(visible))
        .collect::<ApiResult<Vec<_>>>()?;
    let downloads = artifacts(&s)?
        .into_iter()
        .map(|a| a.descriptor(None))
        .collect::<Vec<_>>();
    Ok(Json(json!({"pairings":rows,"downloads":downloads})))
}
async fn read(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(envelope(&s, authorized_pairing(&s, &h, &id, false)?)?))
}
async fn cancel(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let actor = auth::check(&s, &h, "provision", true)?.actor;
    let p = {
        let db = s.db.lock().unwrap();
        let (owner, mut p) = pairing(&db, &id)?;
        if owner != actor {
            return Err(ApiError::forbidden());
        }
        if p["state"] == "waiting" {
            p["state"] = json!("cancelled");
            p["code"] = Value::Null;
            save_pairing(&db, &id, &p)?;
        }
        p
    };
    Ok(Json(envelope(&s, p)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    schema: u8,
    observatory_id: String,
    code: String,
    request_id: String,
    machine_id: String,
    machine_credential: String,
    name: String,
    os: String,
    arch: String,
    helper_version: String,
}
fn metadata(name: &str, os: &str, arch: &str, version: &str) -> ApiResult<()> {
    if name.trim().is_empty()
        || name.len() > 160
        || name.chars().any(char::is_control)
        || !["windows", "macos", "linux"].contains(&os)
        || !["x86_64", "aarch64"].contains(&arch)
        || version.is_empty()
        || version.len() > 40
        || !version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-+".contains(&c))
    {
        return Err(ApiError::bad(
            "The helper reported unsupported computer details",
        ));
    }
    Ok(())
}
fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| *c != '-' && *c != ' ')
        .flat_map(char::to_uppercase)
        .collect()
}
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |d, (a, b)| d | (a ^ b)) == 0
}
fn claim_response(s: &AppState, id: &str, host: &str, machine: &str) -> Value {
    json!({"schema":1,"pairing_id":id,"observatory_id":s.config.id,"host_id":host,"machine_id":machine,"access":["observe"],"heartbeat_seconds":15})
}
async fn redeem(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(b): Json<Claim>,
) -> ApiResult<Json<Value>> {
    uuid(&id)?;
    uuid(&b.request_id)?;
    uuid(&b.machine_id)?;
    metadata(&b.name, &b.os, &b.arch, &b.helper_version)?;
    if b.schema != 1
        || b.observatory_id != s.config.id
        || b.code.len() > 32
        || b.machine_credential.len() != 64
        || !b.machine_credential.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(ApiError::bad("This setup does not match this Observatory"));
    }
    let hash = auth::digest(&b.machine_credential);
    let _inventory = s.inventory_registry_lock.lock().unwrap();
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    let (_, mut p) = pairing(&tx, &id)?;
    if p["state"] == "paired" {
        let host = p["host_id"].as_str().ok_or_else(ApiError::missing)?;
        let grant: Option<(String, Option<i64>, String)> = tx
            .query_row(
                "SELECT credential_hash,revoked_at,pairing_id FROM machine_grants WHERE host_id=?1",
                [host],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (current, revoked, accepted_pairing) = grant.ok_or_else(ApiError::forbidden)?;
        if revoked.is_some() || accepted_pairing != id {
            return Err(ApiError::forbidden());
        }
        if p["machine_id"] != b.machine_id
            || p["claim_request_id"] != b.request_id
            || p["claim_hash"] != hash
            || !same(&current, &hash)
        {
            return Err(conflict("This code has already paired a different request"));
        }
        return Ok(Json(claim_response(&s, &id, host, &b.machine_id)));
    }
    if p["state"] == "locked" {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many attempts. Get a new code from Observatory".into(),
        ));
    }
    if p["state"] != "waiting" || p["expires_at"].as_i64().unwrap_or(0) <= now_ms() {
        return Err(ApiError(
            StatusCode::GONE,
            "This code expired or was cancelled. Get a new code from Observatory".into(),
        ));
    }
    if !same(
        &normalize_code(&b.code),
        &normalize_code(p["code"].as_str().unwrap_or("")),
    ) {
        let n = p["failures"].as_u64().unwrap_or(0) + 1;
        p["failures"] = json!(n);
        if n >= 5 {
            p["state"] = json!("locked");
            p["code"] = Value::Null;
        }
        save_pairing(&tx, &id, &p)?;
        tx.commit()?;
        return Err(ApiError(
            if n >= 5 {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::UNAUTHORIZED
            },
            "That code did not match. Check the code in Observatory".into(),
        ));
    }
    let old: Option<(String, String, Option<i64>)> = tx
        .query_row(
            "SELECT host_id,credential_hash,revoked_at FROM machine_grants WHERE machine_id=?1",
            [&b.machine_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if old
        .as_ref()
        .is_some_and(|(_, h, r)| r.is_none() && !same(h, &hash))
    {
        return Err(conflict(
            "This computer already has a pairing. Disconnect it before replacing access",
        ));
    }
    let host_id = old
        .as_ref()
        .map(|v| v.0.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let previous: Option<String> = tx
        .query_row(
            "SELECT value FROM records WHERE kind='hosts' AND id=?1",
            [&host_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(raw) = previous {
        let previous = decode(raw)?;
        if previous["transport"] != "paired" || previous["machine_id"] != b.machine_id {
            return Err(conflict("The saved computer identity has changed"));
        }
    } else {
        let host = json!({"id":host_id,"label":b.name.trim(),"transport":"paired","address":null,"install_roots":[],"recipes":[],"machine_id":b.machine_id,"revision":1,"created_at":now_ms(),"updated_at":now_ms()});
        tx.execute(
            "INSERT INTO records VALUES('hosts',?1,?2)",
            params![host_id, host.to_string()],
        )?;
    }
    tx.execute("INSERT INTO machine_grants VALUES(?1,?2,?3,NULL,?4,?5) ON CONFLICT(host_id) DO UPDATE SET credential_hash=excluded.credential_hash,revoked_at=NULL,paired_at=excluded.paired_at,pairing_id=excluded.pairing_id",params![host_id,b.machine_id,hash,now_ms(),id])?;
    // A claim alone is not a heartbeat. Never reuse a revoked grant's old online observation.
    tx.execute(
        "DELETE FROM machine_observations WHERE host_id=?1",
        [&host_id],
    )?;
    p["state"] = json!("paired");
    p["code"] = Value::Null;
    p["host_id"] = json!(host_id);
    p["machine_id"] = json!(b.machine_id);
    p["claim_hash"] = json!(hash);
    p["claim_request_id"] = json!(b.request_id);
    save_pairing(&tx, &id, &p)?;
    tx.execute(
        "INSERT INTO audit(at,kind,resource_id,data) VALUES(?1,'machine.paired',?2,?3)",
        params![
            now_ms(),
            host_id,
            json!({"pairing_id":id,"access":["observe"]}).to_string()
        ],
    )?;
    tx.commit()?;
    Ok(Json(claim_response(&s, &id, &host_id, &b.machine_id)))
}
fn bearer(h: &HeaderMap) -> ApiResult<String> {
    let t = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(ApiError::unauthorized)?;
    if t.len() != 64 || !t.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::unauthorized());
    }
    Ok(auth::digest(t))
}
fn grant(db: &Connection, id: &str, hash: &str, allow_revoked: bool) -> ApiResult<String> {
    let row: Option<(String, String, Option<i64>)> = db
        .query_row(
            "SELECT machine_id,credential_hash,revoked_at FROM machine_grants WHERE host_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (machine, stored, revoked) = row.ok_or_else(ApiError::unauthorized)?;
    if !same(&stored, hash) {
        return Err(ApiError::unauthorized());
    }
    if !allow_revoked && revoked.is_some() {
        return Err(ApiError::forbidden());
    }
    Ok(machine)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Beat {
    schema: u8,
    machine_id: String,
    name: String,
    os: String,
    arch: String,
    helper_version: String,
}
async fn heartbeat(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<Beat>,
) -> ApiResult<Json<Value>> {
    metadata(&b.name, &b.os, &b.arch, &b.helper_version)?;
    let hash = bearer(&h)?;
    let db = s.db.lock().unwrap();
    let machine = grant(&db, &id, &hash, false)?;
    if b.schema != 1 || b.machine_id != machine {
        return Err(ApiError::forbidden());
    }
    let value = json!({"id":machine,"os":b.os,"arch":b.arch,"helper_version":b.helper_version,"reported_name":b.name,"last_seen_at":now_ms()});
    db.execute("INSERT INTO machine_observations VALUES(?1,?2) ON CONFLICT(host_id) DO UPDATE SET value=excluded.value",params![id,value.to_string()])?;
    Ok(Json(
        json!({"schema":1,"observatory_id":s.config.id,"host_id":id,"state":"connected","access":["observe"],"heartbeat_seconds":15}),
    ))
}
async fn revoke(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let admin = auth::check(&s, &h, "provision", true).is_ok();
    revoke_grant(&s, &h, &id, admin)
}
async fn revoke_self(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    revoke_grant(&s, &h, &id, false)
}
fn revoke_grant(s: &AppState, h: &HeaderMap, id: &str, admin: bool) -> ApiResult<Json<Value>> {
    // The helper-only listener never accepts the Observatory's management credential.
    let hash = if admin { None } else { Some(bearer(h)?) };
    let _inventory = s.inventory_registry_lock.lock().unwrap();
    let db = s.db.lock().unwrap();
    if let Some(hash) = hash {
        grant(&db, id, &hash, true)?;
    }
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM machine_grants WHERE host_id=?1)",
        [&id],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(ApiError::missing());
    }
    let changed = db.execute(
        "UPDATE machine_grants SET revoked_at=?2 WHERE host_id=?1 AND revoked_at IS NULL",
        params![id, now_ms()],
    )?;
    if changed != 0 {
        db.execute(
            "INSERT INTO audit(at,kind,resource_id,data) VALUES(?1,'machine.revoked',?2,?3)",
            params![
                now_ms(),
                id,
                json!({"by":if admin{"observatory"}else{"computer"}}).to_string()
            ],
        )?;
    }
    Ok(Json(json!({"host_id":id,"state":"disconnected"})))
}
pub fn project_hosts(s: &AppState, mut hosts: Vec<Value>) -> ApiResult<Vec<Value>> {
    let db = s.db.lock().unwrap();
    for host in &mut hosts {
        if host["transport"] != "paired" {
            continue;
        }
        let id = host["id"].as_str().unwrap_or("");
        let g: Option<(String, Option<i64>, i64)> = db
            .query_row(
                "SELECT machine_id,revoked_at,paired_at FROM machine_grants WHERE host_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((machine, revoked, paired)) = g else {
            host["machine"] = json!({"state":"disconnected"});
            continue;
        };
        let raw: Option<String> = db
            .query_row(
                "SELECT value FROM machine_observations WHERE host_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let mut m = raw.map(decode).transpose()?.unwrap_or(json!({}));
        m["id"] = json!(machine);
        m["access"] = json!(["observe"]);
        m["paired_at"] = json!(paired);
        m["revoked_at"] = json!(revoked);
        m["state"] = json!(if revoked.is_some() {
            "disconnected"
        } else if m["last_seen_at"]
            .as_i64()
            .is_some_and(|v| v <= now_ms() && now_ms() - v <= FRESH)
        {
            "online"
        } else {
            "offline"
        });
        host["machine"] = m;
    }
    Ok(hosts)
}
pub fn check_removable(s: &AppState, id: &str) -> ApiResult<()> {
    let active: bool = s.db.lock().unwrap().query_row(
        "SELECT EXISTS(SELECT 1 FROM machine_grants WHERE host_id=?1 AND revoked_at IS NULL)",
        [id],
        |r| r.get(0),
    )?;
    if active {
        return Err(conflict(
            "Disconnect this computer's pairing before removing it. Agents keep working",
        ));
    }
    Ok(())
}
pub fn edit_label(s: &AppState, id: &str, body: &Value) -> ApiResult<Value> {
    let mut old = s.read("hosts", id)?;
    let label = body["label"].as_str().unwrap_or("").trim();
    if label.is_empty() || label.len() > 160 || label.chars().any(char::is_control) {
        return Err(ApiError::bad("Give this computer a short name"));
    }
    let object = body
        .as_object()
        .ok_or_else(|| ApiError::bad("Expected computer settings"))?;
    for (k, v) in object {
        if ["label", "expected_revision"].contains(&k.as_str()) {
            continue;
        }
        if !["transport", "address", "install_roots", "recipes"].contains(&k.as_str())
            || old.get(k) != Some(v)
        {
            return Err(ApiError::bad(
                "Pairing does not authorize changing computer access or installers",
            ));
        }
    }
    let expected = body["expected_revision"]
        .as_u64()
        .ok_or_else(|| ApiError::bad("Refresh before changing this computer"))?;
    old["label"] = json!(label);
    s.save("hosts", old, Some(expected))
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    platform: String,
    label: String,
    architecture: String,
    filename: String,
    sha256: String,
    version: String,
}
impl Artifact {
    fn descriptor(&self, id: Option<&str>) -> Value {
        json!({"platform":self.platform,"label":self.label,"architecture":self.architecture,"url":id.map(|id|format!("/api/v1/machine-pairings/{id}/setup/{}",self.platform)),"filename":format!("Constellation-Computer-{}.zip",self.platform),"version":self.version})
    }
}
fn artifact_path(s: &AppState, a: &Artifact) -> PathBuf {
    s.home.join("machine-helper-artifacts").join(&a.filename)
}
fn artifacts(s: &AppState) -> ApiResult<Vec<Artifact>> {
    let manifest = s.home.join("machine-helper-artifacts/manifest.json");
    if !manifest.exists() {
        return Ok(vec![]);
    }
    let raw: Value = serde_json::from_slice(&std::fs::read(manifest)?)
        .map_err(|_| ApiError::bad("Computer helper downloads need repair"))?;
    let all: Vec<Artifact> = serde_json::from_value(raw["artifacts"].clone())
        .map_err(|_| ApiError::bad("Computer helper downloads need repair"))?;
    if raw["schema"] != 1 || all.len() > 6 {
        return Err(ApiError::bad("Invalid helper manifest"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut ready = vec![];
    for a in all {
        if ![
            "windows-x86_64",
            "windows-aarch64",
            "macos-aarch64",
            "macos-x86_64",
            "linux-aarch64",
            "linux-x86_64",
        ]
        .contains(&a.platform.as_str())
            || !seen.insert(a.platform.clone())
            || a.filename.contains(['/', '\\'])
            || !a.filename.ends_with(".zip")
            || a.sha256.len() != 64
            || a.version.len() > 40
            || a.label.len() > 80
            || a.architecture.len() > 80
        {
            return Err(ApiError::bad("Invalid helper download"));
        }
        let file = artifact_path(s, &a);
        if file.is_file() && std::fs::metadata(&file)?.len() <= 80 * 1024 * 1024 {
            use sha2::{Digest, Sha256};
            if format!("{:x}", Sha256::digest(std::fs::read(file)?)) == a.sha256 {
                ready.push(a);
            }
        }
    }
    Ok(ready)
}
async fn download(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((id, platform)): Path<(String, String)>,
) -> ApiResult<Response> {
    let p = visible(authorized_pairing(&s, &h, &id, false)?);
    if p["state"] != "waiting" {
        return Err(ApiError(
            StatusCode::GONE,
            "Get a new code before downloading setup".into(),
        ));
    }
    let a = artifacts(&s)?
        .into_iter()
        .find(|a| a.platform == platform)
        .ok_or_else(ApiError::missing)?;
    let (endpoint, ca) = crate::machine_network::endpoint(&s).await?;
    let bootstrap = json!({"schema":1,"observatory_url":endpoint,"observatory_id":s.config.id,"observatory_name":format!("{}'s constellation",s.config.owner),"pairing_id":id,"expires_at":p["expires_at"],"ca_pem":ca});
    let file = artifact_path(&s, &a);
    let filename = format!("attachment; filename=\"Constellation-Computer-{platform}.zip\"");
    let data = tokio::task::spawn_blocking(move || {
        setup_archive(file, &a.sha256, &a.platform, &bootstrap)
    })
    .await
    .map_err(|_| ApiError::bad("Could not prepare setup"))??;
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, filename),
        ],
        data,
    )
        .into_response())
}
fn setup_archive(
    file: PathBuf,
    expected: &str,
    platform: &str,
    bootstrap: &Value,
) -> ApiResult<Vec<u8>> {
    use sha2::{Digest, Sha256};
    if std::fs::metadata(&file)?.len() > 80 * 1024 * 1024 {
        return Err(ApiError::bad("Helper archive is too large"));
    }
    let data = std::fs::read(file)?;
    if format!("{:x}", Sha256::digest(&data)) != expected {
        return Err(ApiError::bad("Helper download failed its integrity check"));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(&data))
        .map_err(|_| ApiError::bad("Invalid helper archive"))?;
    if archive.len() > 50 {
        return Err(ApiError::bad("Invalid helper archive"));
    }
    for i in 0..archive.len() {
        let f = archive
            .by_index(i)
            .map_err(|_| ApiError::bad("Invalid helper archive"))?;
        let name = f
            .name()
            .map_err(|_| ApiError::bad("Invalid helper archive name"))?;
        if f.enclosed_name().is_none()
            || name.contains(['\\', ':'])
            || name
                .rsplit('/')
                .next()
                .is_some_and(|n| n.eq_ignore_ascii_case("observatory-pairing.json"))
            || f.size() > 80 * 1024 * 1024
            || f.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
        {
            return Err(ApiError::bad("Unsafe helper archive"));
        }
    }
    drop(archive);
    let mut zip = zip::ZipWriter::new_append(Cursor::new(data))
        .map_err(|_| ApiError::bad("Invalid helper archive"))?;
    zip.start_file(
        "observatory-pairing.json",
        zip::write::SimpleFileOptions::default().unix_permissions(0o600),
    )
    .map_err(|_| ApiError::bad("Could not prepare setup"))?;
    zip.write_all(serde_json::to_string_pretty(bootstrap).unwrap().as_bytes())?;
    if platform.starts_with("macos-") {
        // The current unsigned preview must survive Finder app translocation,
        // which does not preserve a sibling config file. Signed distribution
        // needs a separately opened setup file; never rewrite a signed bundle.
        zip.start_file(
            "Constellation Computer.app/Contents/Resources/observatory-pairing.json",
            zip::write::SimpleFileOptions::default().unix_permissions(0o600),
        )
        .map_err(|_| ApiError::bad("Could not prepare Mac setup"))?;
        zip.write_all(serde_json::to_string_pretty(bootstrap).unwrap().as_bytes())?;
    }
    Ok(zip
        .finish()
        .map_err(|_| ApiError::bad("Could not prepare setup"))?
        .into_inner())
}
