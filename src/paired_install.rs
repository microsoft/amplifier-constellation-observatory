//! The optional computer helper runs one pinned Instance installer. It does not
//! execute browser commands or turn machine presence into installation authority.
use crate::{
    auth, config,
    error::{ApiError, ApiResult},
    paired_protocol::{self, InstallRequest, InstallResult},
    provider,
    state::{AppState, now_ms},
};
use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::{net::IpAddr, sync::Arc};

pub(crate) fn access(db: &rusqlite::Connection, id: &str) -> ApiResult<Value> {
    let raw: Option<String> = db.query_row(
        "SELECT p.value FROM machine_grants g JOIN machine_pairings p ON p.id=g.pairing_id WHERE g.host_id=?1 AND g.revoked_at IS NULL",
        [id], |r| r.get(0)).optional()?;
    let p: Value = serde_json::from_str(&raw.ok_or_else(ApiError::forbidden)?)
        .map_err(|_| ApiError::bad("Saved computer access needs inspection"))?;
    Ok(if p["installation_granted"] == true {
        json!(["observe", paired_protocol::INSTALL_ACCESS])
    } else {
        json!(["observe"])
    })
}
pub(crate) fn enabled(db: &rusqlite::Connection, id: &str) -> ApiResult<bool> {
    Ok(access(db, id)?
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == paired_protocol::INSTALL_ACCESS)))
}
fn current_request(s: &AppState, request: &InstallRequest) -> ApiResult<bool> {
    let db = s.db.lock().unwrap();
    let host = request.host_id.to_string();
    if !enabled(&db, &host)? {
        return Ok(false);
    }
    let binding: (String, String) = db.query_row(
        "SELECT pairing_id,machine_id FROM machine_grants WHERE host_id=?1 AND revoked_at IS NULL",
        [&host],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(binding.0 == request.pairing_id.to_string()
        && binding.1 == request.machine_id.to_string()
        && request.observatory_id.to_string() == s.config.id)
}
pub fn plan(s: &AppState, host: &Value, body: &Value) -> ApiResult<Value> {
    if body.as_object().is_none_or(|m| {
        m.keys().any(|k| {
            ![
                "host_id",
                "recipe_id",
                "name",
                "request_id",
                "plan_digest",
                "home",
                "port",
            ]
            .contains(&k.as_str())
        })
    }) {
        return Err(ApiError::bad("Use the fixed agent setup fields"));
    }
    if body["recipe_id"] != paired_protocol::RECIPE
        || !body["home"].is_null()
        || body["port"].as_u64().unwrap_or(0) != 0
    {
        return Err(ApiError::bad(
            "This computer chooses its own supported installer and agent folder",
        ));
    }
    let id = body["request_id"]
        .as_str()
        .and_then(|v| uuid::Uuid::parse_str(v).ok())
        .ok_or_else(|| ApiError::bad("Start a new installation request"))?;
    let host_id = host["id"].as_str().ok_or_else(ApiError::missing)?;
    let (pairing, observation) = {
        let db = s.db.lock().unwrap();
        if !enabled(&db, host_id)? {
            return Err(ApiError::forbidden());
        }
        let pairing: String = db.query_row(
            "SELECT pairing_id FROM machine_grants WHERE host_id=?1 AND revoked_at IS NULL",
            [host_id],
            |r| r.get(0),
        )?;
        let raw: String = db.query_row(
            "SELECT value FROM machine_observations WHERE host_id=?1",
            [host_id],
            |r| r.get(0),
        )?;
        (
            pairing,
            serde_json::from_str::<Value>(&raw)
                .map_err(|_| ApiError::bad("Computer observation needs inspection"))?,
        )
    };
    if observation["installation_protocol"] != 1 {
        return Err(ApiError::bad(
            "Open the current computer helper to enable agent setup",
        ));
    }
    let target = paired_protocol::target(
        observation["os"].as_str().unwrap_or(""),
        observation["arch"].as_str().unwrap_or(""),
    )
    .map_err(|e| ApiError::bad(e.to_string()))?;
    let bind_address = observation["peer_address"]
        .as_str()
        .and_then(|s| s.parse::<IpAddr>().ok())
        .ok_or_else(|| {
            ApiError::bad("Reconnect this computer so its network address can be checked")
        })?;
    let lock = paired_protocol::package_lock();
    let request = InstallRequest {
        schema: 1,
        id,
        observatory_id: uuid::Uuid::parse_str(&s.config.id)
            .map_err(|_| ApiError::bad("Invalid Observatory identity"))?,
        host_id: uuid::Uuid::parse_str(host_id).unwrap(),
        machine_id: uuid::Uuid::parse_str(host["machine_id"].as_str().unwrap_or(""))
            .map_err(|_| ApiError::bad("Invalid computer identity"))?,
        pairing_id: uuid::Uuid::parse_str(&pairing).unwrap(),
        name: body["name"].as_str().unwrap_or("").trim().into(),
        target: target.clone(),
        package_sha256: lock["targets"][&target]["sha256"].as_str().unwrap().into(),
        bind_address,
    };
    request
        .validate()
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let mut plan = json!({"contract":"agent-observatory.host-plan/1","host_id":host_id,"host_revision":host["revision"],
        "recipe_id":paired_protocol::RECIPE,"version":lock["version"],"name":request.name,"transport":"paired",
        "home":"Chosen by this computer","request":request,"authority":"paired computer installation grant"});
    plan["plan_digest"] = json!(auth::digest(&plan.to_string()));
    Ok(plan)
}

/// Re-delivery is the same immutable request. The helper's durable request
/// record determines whether it can start; an accepted request is never rerun.
pub(crate) fn offer(s: &AppState, host: &str) -> ApiResult<Option<InstallRequest>> {
    let _gate = s.provision_gate.lock().unwrap();
    if !enabled(&s.db.lock().unwrap(), host)? {
        return Ok(None);
    }
    let pairing: String = s.db.lock().unwrap().query_row(
        "SELECT pairing_id FROM machine_grants WHERE host_id=?1 AND revoked_at IS NULL",
        [host],
        |r| r.get(0),
    )?;
    let mut jobs = s.list("jobs")?;
    jobs.sort_by_key(|j| j["created_at"].as_i64().unwrap_or(0));
    for mut job in jobs {
        if job["plan"]["host_id"] != host
            || job["plan"]["transport"] != "paired"
            || job["remote_result"].is_object()
            || !["queued", "running", "interrupted"].contains(&job["status"].as_str().unwrap_or(""))
        {
            continue;
        }
        let request: InstallRequest = serde_json::from_value(job["plan"]["request"].clone())
            .map_err(|_| ApiError::bad("Saved installation request needs inspection"))?;
        if request.pairing_id.to_string() != pairing {
            continue;
        }
        request
            .validate()
            .map_err(|e| ApiError::bad(e.to_string()))?;
        job["status"] = json!("running");
        job["note"] = json!("The computer is handling this saved installation request.");
        s.save("jobs", job, None)?;
        return Ok(Some(request));
    }
    Ok(None)
}

pub async fn complete(s: Arc<AppState>, host: &str, result: InstallResult) -> ApiResult<Value> {
    result
        .validate()
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let id = result.request.id.to_string();
    {
        let _gate = s.provision_gate.lock().unwrap();
        if !current_request(&s, &result.request)? {
            return Err(ApiError::forbidden());
        }
        let job = s.read("jobs", &id)?;
        if result.request.host_id.to_string() != host
            || job["plan"]["request"] != json!(result.request)
        {
            return Err(ApiError::bad(
                "The installation result does not match this computer and request",
            ));
        }
        let path = s.home.join("jobs").join(format!("{id}.remote.json"));
        let bytes = serde_json::to_vec(&result).unwrap();
        if path.exists() {
            if std::fs::read(&path)? != bytes {
                return Err(ApiError::bad(
                    "This installation already has a different recorded outcome",
                ));
            }
        } else {
            config::private_create(&path, &bytes)?;
        }
        if job["remote_result"].is_object() {
            return Ok(json!({"accepted":true,"request_id":id}));
        }
        let mut job = job;
        job["status"] = json!(if result.state == "unknown" {
            "interrupted"
        } else {
            &result.state
        });
        job["remote_result"] =
            json!({"state":result.state,"request_digest":result.request.digest()});
        job["exit_code"] = if result.state == "completed" {
            json!(0)
        } else {
            Value::Null
        };
        job["note"] = json!(result.message);
        job["completed_at"] = json!(now_ms());
        job["connection_pending"] = json!(result.state == "completed");
        s.save("jobs", job, None)?;
    }
    if result.state == "completed" {
        let job = s.read("jobs", &id)?;
        let outcome = attach(&s, &job).await;
        let mut current = s.read("jobs", &id)?;
        match outcome {
            Ok(endpoint) => {
                current["endpoint_id"] = endpoint["id"].clone();
                current["connection_pending"] = json!(false);
                current["note"] = json!(
                    "Your agent is installed and connected. Choose its model access and places."
                );
            }
            Err(error) => {
                current["note"] = json!(format!(
                    "The agent is installed. Finishing its connection needs attention: {error}"
                ));
            }
        }
        s.save("jobs", current, None)?;
    }
    Ok(json!({"accepted":true,"request_id":id}))
}

pub async fn attach(s: &AppState, job: &Value) -> Result<Value> {
    let id = job["id"]
        .as_str()
        .context("Missing installation identity")?;
    let result: InstallResult = serde_json::from_slice(&std::fs::read(
        s.home.join("jobs").join(format!("{id}.remote.json")),
    )?)?;
    result.validate()?;
    ensure!(
        job["plan"]["request"] == json!(result.request),
        "The saved result is for another installation"
    );
    ensure!(
        current_request(s, &result.request).map_err(|e| anyhow::anyhow!(e.1))?,
        "Computer access was disconnected"
    );
    let a = result
        .attachment
        .context("The installation is not confirmed")?;
    let bridge = s.home.join("support/instance");
    let (manifest, _) = crate::instance_package::inventory(&bridge)?;
    let client_home = s.home.join("paired-agents").join(id);
    config::private_dir(&client_home)?;
    config::private_dir(&client_home.join("tls"))?;
    let certificate = client_home.join("tls/cert.pem");
    let credential = format!("paired-{id}");
    for (path, bytes) in [
        (certificate.clone(), a.ca_pem.as_bytes()),
        (
            s.home.join("secrets").join(&credential),
            a.credential.as_bytes(),
        ),
    ] {
        if path.exists() {
            ensure!(std::fs::read(&path)? == bytes, "Saved agent access changed");
        } else {
            config::private_create(&path, bytes)?;
        }
    }
    let mut acp_url = reqwest::Url::parse(&a.origin)?;
    acp_url.set_scheme("wss").unwrap();
    acp_url.set_path("/api/v1/acp");
    let endpoint = json!({"id":id,"kind":"agent","label":result.request.name,"host_id":result.request.host_id,"ui_url":a.origin,
        "management":{"provider":"agent-instance","base_url":a.origin,"expected_id":a.instance_id,"ca_file":certificate,"credential_ref":credential,"access":"manage"},
        "acp":{"transport":"stdio","command":bridge.join(manifest["binary"].as_str().unwrap()),"cwd":client_home,"session_cwd":a.session_cwd,
            "args":["--home",client_home,"acp","--url",acp_url.as_str(),"--token-file",s.home.join("secrets").join(&credential)]}});
    provider::validate(&endpoint).map_err(|e| anyhow::anyhow!(e.1))?;
    let observed = provider::inspect(s, &endpoint)
        .await
        .map_err(|e| anyhow::anyhow!(e.1))?;
    ensure!(
        observed["instance_id"] == a.instance_id.to_string(),
        "A different agent answered"
    );
    let _inventory = s.inventory_registry_lock.lock().unwrap();
    ensure!(
        current_request(s, &result.request).map_err(|e| anyhow::anyhow!(e.1))?,
        "Computer access changed before attachment"
    );
    let saved = match s.read("endpoints", id) {
        Ok(existing) => {
            ensure!(
                existing["management"] == endpoint["management"],
                "This installation has another connection"
            );
            existing
        }
        Err(error) if error.0 == axum::http::StatusCode::NOT_FOUND => s
            .save("endpoints", endpoint, Some(0))
            .map_err(|e| anyhow::anyhow!(e.1))?,
        Err(error) => return Err(anyhow::anyhow!(error.1)),
    };
    s.save("observations", observed, None)
        .map_err(|e| anyhow::anyhow!(e.1))?;
    Ok(saved)
}
