//! Optional fleet client of each installation's authoritative approval policy.
//! A durable intent precedes every PUT. Neither restart nor request replay sends
//! it again; uncertain outcomes can only acquire a separate current observation.
use crate::{
    auth,
    error::{ApiError, ApiResult},
    provider,
    state::{AppState, now_ms},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const RECORDS: &str = "approval_policy_operations";
const CAPABILITY: &str = "agent-instance.approval-policy/1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    endpoint_id: String,
    endpoint_revision: u64,
    instance_id: Option<String>,
    policy_revision: Option<u64>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Apply {
    request_id: String,
    mode: String,
    targets: Vec<Target>,
}

fn agents(s: &AppState) -> ApiResult<Vec<Value>> {
    let mut all: Vec<_> = s
        .list("endpoints")?
        .into_iter()
        .filter(|e| e["kind"] == "agent")
        .collect();
    all.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(all)
}
fn row(e: &Value) -> Value {
    json!({"endpoint_id":e["id"],"label":e["label"],
        "endpoint_revision":e["revision"],"instance_id":e["management"]["expected_id"],
        "policy_revision":null,"mode":null,"status":"pending","detail":"Waiting to inspect this agent."})
}
fn failure(row: &mut Value, e: ApiError, mutating: bool) {
    row["status"] = json!(match e.0.as_u16() {
        401 | 403 => "access-required",
        404 | 405 | 501 => "unsupported",
        400 | 409 | 422 => "conflict",
        _ if mutating => "unknown",
        _ => "offline",
    });
    row["detail"] = json!(e.1);
}
fn eligibility(e: &Value) -> Result<(), (&'static str, &'static str)> {
    if e["management"]["provider"] != "agent-instance" {
        return Err((
            "unsupported",
            "This agent does not expose the supported Instance policy interface.",
        ));
    }
    if e["management"]["access"] != "manage" {
        return Err((
            "access-required",
            "This connection has observation access only.",
        ));
    }
    if e["management"]["expected_id"]
        .as_str()
        .is_none_or(str::is_empty)
    {
        return Err((
            "access-required",
            "Inspect and pin this installation identity first.",
        ));
    }
    Ok(())
}
fn valid_policy(p: &Value) -> bool {
    p["schema"] == CAPABILITY
        && p["revision"].as_u64().is_some()
        && matches!(p["mode"].as_str(), Some("ask" | "automatic"))
        && p["delegated_clients"].as_array().is_some_and(|a| {
            a.len() <= 128
                && a.iter()
                    .all(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 128))
                && a.iter()
                    .filter_map(Value::as_str)
                    .collect::<BTreeSet<_>>()
                    .len()
                    == a.len()
        })
}
async fn policy(s: &AppState, e: &Value) -> ApiResult<Value> {
    let p = provider::call(s, e, "approval-policy", None).await?;
    if !valid_policy(&p) {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "The agent returned an unrecognized approval policy.".into(),
        ));
    }
    Ok(p)
}
fn observed(row: &mut Value, p: &Value) {
    row["policy_revision"] = p["revision"].clone();
    row["mode"] = p["mode"].clone();
}
async fn inspect(s: &AppState, e: &Value) -> Value {
    let mut result = row(e);
    if let Err((status, detail)) = eligibility(e) {
        result["status"] = json!(status);
        result["detail"] = json!(detail);
        return result;
    }
    match policy(s, e).await {
        Ok(p) => {
            observed(&mut result, &p);
            result["status"] = json!("ready");
            result["detail"] = json!("Current policy read from this agent.");
        }
        Err(e) => failure(&mut result, e, false),
    }
    result
}

// Deduplicate successfully inspected aliases using a stable endpoint ID. A
// failing alias must retain its identity/access error, even if another works.
fn deduplicate(rows: &mut [Value]) {
    let mut primary: BTreeMap<String, String> = BTreeMap::new();
    for row in rows.iter() {
        if row["policy_revision"].is_null() {
            continue;
        }
        if let Some(id) = row["instance_id"].as_str().filter(|id| !id.is_empty()) {
            let candidate = row["endpoint_id"].as_str().unwrap().to_owned();
            let chosen = primary
                .entry(id.into())
                .or_insert_with(|| candidate.clone());
            if candidate < *chosen {
                *chosen = candidate;
            }
        }
    }
    for row in rows {
        if row["policy_revision"].is_null() {
            continue;
        }
        if let Some(id) = row["instance_id"].as_str().and_then(|id| primary.get(id))
            && row["endpoint_id"] != *id
        {
            row["primary_endpoint_id"] = json!(id);
            row["status"] = json!("duplicate");
            row["detail"] = json!("Another saved connection represents the same installed agent.");
        }
    }
}
fn public(mut op: Value) -> Value {
    op.as_object_mut().unwrap().remove("request");
    op
}
fn latest(s: &AppState) -> ApiResult<Option<Value>> {
    Ok(s.list(RECORDS)?
        .into_iter()
        .filter(|op| op["default_revision"].is_null())
        .max_by_key(|op| op["created_at"].as_i64())
        .map(public))
}
pub async fn preview(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", false)?;
    let mut tasks = tokio::task::JoinSet::new();
    for e in agents(&s)? {
        let s = s.clone();
        tasks.spawn(async move {
            let Ok(_permit) = s.operation_gate.clone().acquire_owned().await else {
                return row(&e);
            };
            inspect(&s, &e).await
        });
    }
    let mut rows = Vec::new();
    while let Some(result) = tasks.join_next().await {
        rows.push(result.map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The fleet inspection was interrupted; refresh before applying.".into(),
            )
        })?);
    }
    rows.sort_by(|a, b| a["endpoint_id"].as_str().cmp(&b["endpoint_id"].as_str()));
    deduplicate(&mut rows);
    let active: Vec<_> = rows.iter().filter(|r| r["status"] != "duplicate").collect();
    let complete = !active.is_empty() && active.iter().all(|r| r["status"] == "ready");
    let modes: BTreeSet<_> = active.iter().filter_map(|r| r["mode"].as_str()).collect();
    let mode = if !complete {
        "unknown"
    } else if modes.len() == 1 {
        modes.iter().next().unwrap()
    } else {
        "mixed"
    };
    let targets: Vec<_> = rows.iter().map(|r| json!({"endpoint_id":r["endpoint_id"],
        "endpoint_revision":r["endpoint_revision"],"instance_id":r["instance_id"],"policy_revision":r["policy_revision"]})).collect();
    Ok(Json(
        json!({"mode":mode,"complete":complete,"observed_at":now_ms(),"targets":rows,
        "preview":{"targets":targets},"last_operation":latest(&s)?,
        "new_agent_default":default_value(&s)?,"inheritance":s.list(INHERITANCE)?}),
    ))
}
fn conflict(detail: &str) -> ApiError {
    ApiError(StatusCode::CONFLICT, detail.into())
}
fn same_inventory(endpoints: &[Value], targets: &[Target]) -> bool {
    endpoints.len() == targets.len()
        && endpoints.iter().zip(targets).all(|(e, t)| {
            e["id"] == t.endpoint_id
                && e["revision"].as_u64() == Some(t.endpoint_revision)
                && e["management"]["expected_id"].as_str() == t.instance_id.as_deref()
        })
}
pub async fn apply(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(mut request): Json<Apply>,
) -> ApiResult<Json<Value>> {
    let actor = auth::check(&s, &h, "admin", true)?.actor;
    if !uuid::Uuid::parse_str(&request.request_id)
        .is_ok_and(|id| id.to_string() == request.request_id)
        || !matches!(request.mode.as_str(), "ask" | "automatic")
        || request.targets.is_empty()
    {
        return Err(ApiError::bad(
            "Use a request UUID, Ask or Automatic, and the complete preview cohort.",
        ));
    }
    request
        .targets
        .sort_by(|a, b| a.endpoint_id.cmp(&b.endpoint_id));
    let id = request.request_id.clone();
    let value =
        serde_json::to_value(&request).map_err(|_| ApiError::bad("Invalid fleet request"))?;
    let result;
    let permit;
    {
        let _inventory = s.inventory_registry_lock.lock().unwrap();
        let _journal = s.fleet_policy_lock.lock().unwrap();
        match s.read(RECORDS, &id) {
            Ok(old) => {
                return if old["request"] == value {
                    Ok(Json(public(old)))
                } else {
                    Err(conflict(
                        "This request ID already names a different operation.",
                    ))
                };
            }
            Err(e) if e.0 == StatusCode::NOT_FOUND => {}
            Err(e) => return Err(e),
        }
        let endpoints = agents(&s)?;
        if s.list(RECORDS)?.iter().any(|op| op["status"] == "running") {
            return Err(conflict(
                "A fleet policy operation is already running; inspect its saved result.",
            ));
        }
        if !same_inventory(&endpoints, &request.targets) {
            return Err(conflict(
                "The registered agents or connection revisions changed; refresh the whole preview.",
            ));
        }
        permit = s
            .operation_gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::busy())?;
        let mut rows: Vec<_> = endpoints
            .iter()
            .zip(&request.targets)
            .map(|(e, t)| {
                let mut r = row(e);
                r["policy_revision"] = json!(t.policy_revision);
                r
            })
            .collect();
        deduplicate(&mut rows);
        s.audit(
            "approval-policy.submitted",
            Some(&id),
            json!({"actor":actor,"mode":request.mode}),
        )?;
        result = s.save(
            RECORDS,
            json!({"id":id,"mode":request.mode,"status":"running",
            "actor":actor,"request":value,"targets":rows}),
            Some(0),
        )?;
    }
    let worker = s.clone();
    tokio::spawn(async move {
        let _permit = permit;
        if run(&worker, &request).await.is_err() {
            // If persistence fails, the last durable sending intent still survives.
            let _ = update(&worker, &request.request_id, |op| {
                interrupt(op);
            });
        }
    });
    Ok(Json(public(result)))
}
fn update(s: &AppState, id: &str, change: impl FnOnce(&mut Value)) -> ApiResult<Value> {
    let _journal = s.fleet_policy_lock.lock().unwrap();
    let mut record = s.read(RECORDS, id)?;
    let revision = record["revision"].as_u64();
    change(&mut record);
    s.save(RECORDS, record, revision)
}
fn save_row(s: &AppState, id: &str, index: usize, row: Value) -> ApiResult<()> {
    update(s, id, |op| op["targets"][index] = row)?;
    Ok(())
}
// Serialize cancellation of a saved default with the durable dispatch boundary.
fn save_dispatch(s: &AppState, id: &str, index: usize, mut result: Value) -> ApiResult<bool> {
    let _journal = s.fleet_policy_lock.lock().unwrap();
    let mut op = s.read(RECORDS, id)?;
    let revision = op["revision"].as_u64();
    let allowed = match op["default_revision"].as_u64() {
        None => true,
        Some(r) => default_value(s)?["revision"].as_u64() == Some(r),
    };
    if !allowed {
        result["status"] = json!("not-sent");
        result["detail"] = json!("The new-agent default changed before dispatch.");
    }
    op["targets"][index] = result;
    s.save(RECORDS, op, revision)?;
    Ok(allowed)
}
fn finish(op: &mut Value) {
    let rows = op["targets"].as_array().unwrap();
    let succeeded = |r: &Value| matches!(r["status"].as_str(), Some("succeeded" | "unchanged"));
    let complete = !rows.is_empty()
        && rows.iter().all(|r| {
            succeeded(r)
                || (r["status"] == "duplicate"
                    && rows
                        .iter()
                        .any(|p| p["endpoint_id"] == r["primary_endpoint_id"] && succeeded(p)))
        });
    op["status"] = json!(if complete { "complete" } else { "partial" });
    op["finished_at"] = json!(now_ms());
}
fn interrupt(op: &mut Value) {
    for r in op["targets"].as_array_mut().unwrap() {
        match r["status"].as_str() {
            Some("sending") => {
                r["status"] = json!("unknown");
                r["detail"] = json!(
                    "The service stopped after recording dispatch intent. Inspect the current policy; this operation will not be sent again."
                );
            }
            Some("pending") => {
                r["status"] = json!("not-sent");
                r["detail"] = json!(
                    "The operation stopped before dispatch. Refresh before starting another operation."
                );
            }
            _ => {}
        }
    }
    finish(op);
}
pub fn recover(db: &Connection) -> anyhow::Result<()> {
    let mut q = db.prepare(
        "SELECT id,value FROM records WHERE kind=?1 AND json_extract(value,'$.status')='running'",
    )?;
    let rows = q
        .query_map([RECORDS], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, raw) in rows {
        let mut op: Value = serde_json::from_str(&raw)?;
        interrupt(&mut op);
        op["updated_at"] = json!(now_ms());
        db.execute(
            "UPDATE records SET value=?1 WHERE kind=?2 AND id=?3",
            params![op.to_string(), RECORDS, id],
        )?;
    }
    Ok(())
}
async fn run(s: &AppState, request: &Apply) -> ApiResult<()> {
    let id = &request.request_id;
    for (index, target) in request.targets.iter().enumerate() {
        let mut result = s.read(RECORDS, id)?["targets"][index].clone();
        if result["status"] == "duplicate" {
            continue;
        }
        let endpoint = match s.read("endpoints", &target.endpoint_id) {
            Ok(e)
                if e["revision"].as_u64() == Some(target.endpoint_revision)
                    && e["management"]["expected_id"].as_str() == target.instance_id.as_deref() =>
            {
                e
            }
            _ => {
                result["status"] = json!("conflict");
                result["detail"] =
                    json!("This connection changed after the preview; no request was sent.");
                save_row(s, id, index, result)?;
                continue;
            }
        };
        if let Err((status, detail)) = eligibility(&endpoint) {
            result["status"] = json!(status);
            result["detail"] = json!(detail);
            save_row(s, id, index, result)?;
            continue;
        }
        let p = match policy(s, &endpoint).await {
            Ok(p) => p,
            Err(e) => {
                failure(&mut result, e, false);
                save_row(s, id, index, result)?;
                continue;
            }
        };
        observed(&mut result, &p);
        if target.policy_revision != p["revision"].as_u64() {
            result["status"] = json!("conflict");
            result["detail"] = json!(
                "The current policy was not in the approved preview; refresh before changing it."
            );
            save_row(s, id, index, result)?;
            continue;
        }
        if p["mode"] == request.mode {
            result["status"] = json!("unchanged");
            result["detail"] = json!("This agent already uses the requested mode.");
            save_row(s, id, index, result)?;
            continue;
        }
        // Refresh the registry after the remote read, before recording dispatch.
        if s.read("endpoints", &target.endpoint_id)
            .ok()
            .is_none_or(|e| e["revision"].as_u64() != Some(target.endpoint_revision))
        {
            result["status"] = json!("conflict");
            result["detail"] =
                json!("This connection changed during inspection; no request was sent.");
            save_row(s, id, index, result)?;
            continue;
        }
        let payload = json!({"expected_revision":p["revision"],"mode":request.mode,"delegated_clients":p["delegated_clients"]});
        result["status"] = json!("sending");
        result["detail"] = json!("Dispatch intent saved; waiting for the agent's result.");
        if !save_dispatch(s, id, index, result.clone())? {
            continue;
        }
        match provider::call(s, &endpoint, "apply-approval-policy", Some(&payload)).await {
            Ok(reply)
                if valid_policy(&reply)
                    && reply["mode"] == request.mode
                    && reply["delegated_clients"] == p["delegated_clients"]
                    && reply["revision"].as_u64()
                        == p["revision"].as_u64().and_then(|r| r.checked_add(1)) =>
            {
                observed(&mut result, &reply);
                result["status"] = json!("succeeded");
                result["detail"] = json!("This agent confirmed the requested mode.");
            }
            Ok(_) => {
                result["status"] = json!("unknown");
                result["detail"] = json!(
                    "The agent did not confirm the exact policy change. Inspect its current policy; this operation will not be sent again."
                );
            }
            Err(e) => failure(&mut result, e, true),
        }
        save_row(s, id, index, result)?;
    }
    update(s, id, finish)?;
    Ok(())
}
pub async fn operation(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", false)?;
    let old = s.read(RECORDS, &id)?;
    if old["status"] != "running" {
        // Observations never rewrite the dispatch outcome or resend a PUT.
        for (index, r) in old["targets"].as_array().unwrap().iter().enumerate() {
            if r["status"] != "unknown" {
                continue;
            }
            let Some(endpoint_id) = r["endpoint_id"].as_str() else {
                continue;
            };
            let Ok(e) = s.read("endpoints", endpoint_id) else {
                continue;
            };
            if e["revision"] != r["endpoint_revision"]
                || e["management"]["expected_id"] != r["instance_id"]
            {
                continue;
            }
            let Ok(_permit) = s.operation_gate.clone().try_acquire_owned() else {
                continue;
            };
            if let Ok(p) = policy(&s, &e).await {
                update(&s, &id, |op| {
                    op["targets"][index]["observed"] = json!({"mode":p["mode"],
                    "policy_revision":p["revision"],"observed_at":now_ms(),"matches_requested_mode":p["mode"] == old["mode"]})
                })?;
            }
        }
    }
    Ok(Json(public(s.read(RECORDS, &id)?)))
}

const DEFAULTS: &str = "approval_policy_defaults";
const INHERITANCE: &str = "approval_policy_inheritance";
fn read_default(db: &Connection) -> ApiResult<Value> {
    let raw: Option<String> = db
        .query_row(
            "SELECT value FROM records WHERE kind=?1 AND id='new-agents'",
            [DEFAULTS],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match raw {
        Some(raw) => serde_json::from_str(&raw)
            .map_err(|_| ApiError::bad("Invalid saved approval default"))?,
        None => json!({"revision":0,"mode":null}),
    })
}
fn default_value(s: &AppState) -> ApiResult<Value> {
    read_default(&s.db.lock().unwrap())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultChange {
    expected_revision: u64,
    mode: Option<String>,
}
/// Owner preference only. Each agent remains authoritative after the one-time write.
pub async fn set_default(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(change): Json<DefaultChange>,
) -> ApiResult<Json<Value>> {
    let actor = auth::check(&s, &h, "admin", true)?.actor;
    if change
        .mode
        .as_deref()
        .is_some_and(|m| !matches!(m, "ask" | "automatic"))
    {
        return Err(ApiError::bad("Choose Ask, Automatic, or no default."));
    }
    let _inventory = s.inventory_registry_lock.lock().unwrap();
    let _journal = s.fleet_policy_lock.lock().unwrap();
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    let old = read_default(&tx)?;
    if old["revision"].as_u64() != Some(change.expected_revision) {
        return Err(conflict(
            "The new-agent default changed; refresh before saving.",
        ));
    }
    // Existing identities, including aliases, are not newly connected agents.
    let existing: Vec<String> = {
        let mut q = tx.prepare("SELECT value FROM records WHERE kind='endpoints'")?;
        q.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for raw in existing {
        let e: Value =
            serde_json::from_str(&raw).map_err(|_| ApiError::bad("Invalid agent record"))?;
        if let Some(id) = managed_identity(&e) {
            let record = json!({"id":id,"endpoint_id":e["id"],"label":e["label"],"status":"existing","detail":"Existing agent; its current setting is preserved."});
            tx.execute(
                "INSERT OR IGNORE INTO records VALUES(?1,?2,?3)",
                params![INHERITANCE, id, record.to_string()],
            )?;
        }
    }
    let value = json!({"id":"new-agents","revision":change.expected_revision.checked_add(1).ok_or_else(|| ApiError::bad("Revision exhausted"))?,"mode":change.mode,"updated_at":now_ms(),"actor":actor});
    tx.execute("INSERT INTO records VALUES(?1,'new-agents',?2) ON CONFLICT(kind,id) DO UPDATE SET value=excluded.value", params![DEFAULTS,value.to_string()])?;
    tx.execute("INSERT INTO audit(at,kind,resource_id,data) VALUES(?1,'approval-policy.default-changed','new-agents',?2)",params![now_ms(),value.to_string()])?;
    tx.commit()?;
    Ok(Json(value))
}
fn managed_identity(e: &Value) -> Option<&str> {
    if e["kind"] != "agent" || e["management"]["provider"] != "agent-instance" {
        return None;
    }
    e["management"]["expected_id"]
        .as_str()
        .filter(|id| !id.is_empty())
}
/// Called within the endpoint registry transaction, for every registration path.
/// Identity-keyed records prevent re-adding an alias from resetting an owner's choice.
pub fn register(db: &Connection, endpoint: &Value) -> ApiResult<()> {
    let Some(id) = managed_identity(endpoint) else {
        return Ok(());
    };
    let default = read_default(db)?;
    if default["mode"].is_null() {
        return Ok(());
    }
    let record = json!({"id":id,"endpoint_id":endpoint["id"],"endpoint_revision":endpoint["revision"],"label":endpoint["label"],"status":"queued","mode":default["mode"],"default_revision":default["revision"],"operation_id":uuid::Uuid::new_v4().to_string(),"created_at":now_ms()});
    db.execute(
        "INSERT OR IGNORE INTO records VALUES(?1,?2,?3)",
        params![INHERITANCE, id, record.to_string()],
    )?;
    Ok(())
}
/// No network writes are retried. Startup reconciles only already saved operation results.
pub async fn inherit_pending(s: &AppState) -> ApiResult<()> {
    let Ok(_permit) = s.operation_gate.clone().try_acquire_owned() else {
        return Ok(());
    };
    for mut record in s.list(INHERITANCE)? {
        let instance_id = record["id"].as_str().unwrap().to_owned();
        if record["status"] == "applying" {
            let op = s.read(RECORDS, record["operation_id"].as_str().unwrap());
            if let Ok(op) = op {
                if op["status"] != "running" {
                    record["status"] = op["targets"][0]["status"].clone();
                    record["detail"] = op["targets"][0]["detail"].clone();
                    s.save(INHERITANCE, record, None)?;
                }
            } else {
                record["status"] = json!("not-sent");
                record["detail"] = json!(
                    "Registration was interrupted before dispatch; use Apply to all to include this agent."
                );
                s.save(INHERITANCE, record, None)?;
            }
            continue;
        }
        if record["status"] != "queued" {
            continue;
        }
        {
            let _journal = s.fleet_policy_lock.lock().unwrap();
            if s.read(INHERITANCE, &instance_id)?["status"] != "queued" {
                continue;
            }
            if s.list(RECORDS)?.iter().any(|op| op["status"] == "running") {
                return Ok(());
            }
            record["status"] = json!("applying");
            s.save(INHERITANCE, record.clone(), None)?;
        }
        let endpoint = s.read("endpoints", record["endpoint_id"].as_str().unwrap())?;
        let mut result = inspect(s, &endpoint).await;
        if endpoint["revision"] != record["endpoint_revision"]
            || endpoint["management"]["expected_id"] != instance_id
        {
            result["status"] = json!("conflict");
            result["detail"] =
                json!("The agent connection changed before its default could be applied.");
        }
        if default_value(s)?["revision"] != record["default_revision"] {
            result["status"] = json!("not-sent");
            result["detail"] = json!("The new-agent default changed before dispatch.");
        }
        if result["status"] != "ready" {
            record["status"] = result["status"].clone();
            record["detail"] = result["detail"].clone();
            s.save(INHERITANCE, record, None)?;
            continue;
        }
        let request = Apply {
            request_id: record["operation_id"].as_str().unwrap().into(),
            mode: record["mode"].as_str().unwrap().into(),
            targets: vec![Target {
                endpoint_id: endpoint["id"].as_str().unwrap().into(),
                endpoint_revision: endpoint["revision"].as_u64().unwrap(),
                instance_id: Some(instance_id),
                policy_revision: result["policy_revision"].as_u64(),
            }],
        };
        result["status"] = json!("pending");
        s.save(RECORDS,json!({"id":request.request_id,"mode":request.mode,"status":"running","actor":"new-agent-default","default_revision":record["default_revision"],"request":request,"targets":[result]}),Some(0))?;
        if run(s, &request).await.is_err() {
            update(s, &request.request_id, interrupt)?;
        }
        let op = s.read(RECORDS, &request.request_id)?;
        record["status"] = op["targets"][0]["status"].clone();
        record["detail"] = op["targets"][0]["detail"].clone();
        s.save(INHERITANCE, record, None)?;
    }
    Ok(())
}
pub fn start_inheritance(s: &Arc<AppState>) {
    use std::sync::atomic::Ordering;
    if s.inheritance_started.swap(true, Ordering::SeqCst) {
        return;
    }
    let weak = Arc::downgrade(s);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            interval.tick().await;
            let Some(s) = weak.upgrade() else {
                break;
            };
            if *s.shutdown.borrow() {
                break;
            }
            if let Err(e) = inherit_pending(&s).await {
                let _ = s.audit(
                    "approval-policy.inheritance-error",
                    None,
                    json!({"detail":e.1}),
                );
            }
        }
    });
}
