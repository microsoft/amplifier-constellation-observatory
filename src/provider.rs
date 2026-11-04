//! Optional management providers. Ordinary ACP endpoints do not need one.
use crate::{
    config,
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub method: String,
    pub path: String,
    pub access: String,
}
fn op(id: &str, method: &str, path: &str, access: &str) -> Operation {
    Operation {
        id: id.into(),
        method: method.into(),
        path: path.into(),
        access: access.into(),
    }
}
pub fn operations(endpoint: &Value) -> Vec<Operation> {
    let provider = endpoint["management"]["provider"].as_str().unwrap_or("");
    let mut ops = vec![
        op("descriptor", "GET", "descriptor", "observe"),
        op("status", "GET", "status", "observe"),
    ];
    match provider {
        "agent-instance" => ops.extend([
            op("updates", "GET", "updates", "observe"),
            op("request-update", "POST", "updates", "manage"),
            op("update-policy", "PUT", "updates/policy", "manage"),
            op("activity", "GET", "activity", "observe"),
            op("sessions", "GET", "sessions", "observe"),
            op("connections", "GET", "connections", "observe"),
            op("profile-use", "GET", "profile-use", "observe"),
            op("work", "GET", "work", "work"),
            op("organization", "GET", "organization", "work"),
            op("profile-package", "GET", "profile-package", "manage"),
            op("config", "GET", "config", "manage"),
            op("validate-config", "POST", "config/validate", "manage"),
            op("apply-config", "PUT", "config", "manage"),
            op("lifecycle", "POST", "lifecycle", "manage"),
            op("apply-profile", "POST", "profile-package", "manage"),
            op(
                "restore-profile",
                "POST",
                "profile-package/restore",
                "manage",
            ),
            op("apply-organization", "PUT", "organization", "manage"),
            op("create-work", "POST", "work", "manage"),
            op("import-work", "POST", "work/import", "manage"),
            op("context-providers", "GET", "context/providers", "manage"),
            op(
                "apply-context-providers",
                "PUT",
                "context/providers",
                "manage",
            ),
            op("backend", "GET", "backend", "observe"),
            op("install-backend", "POST", "backend/install", "manage"),
            op("memory", "GET", "memory", "memory:read"),
            op("remember", "POST", "memory", "memory"),
            op("memory-item", "GET", "memory/{id}", "memory:read"),
            op("memory-command", "POST", "memory/{id}/commands", "memory"),
            op("scheduler", "GET", "scheduler", "work"),
            op("configure-scheduler", "PUT", "scheduler", "work:execute"),
            op("work-item", "GET", "work/{id}", "work"),
            op("work-command", "POST", "work/{id}/commands", "manage"),
            op("work-execution", "GET", "work/{id}/execution", "work"),
            op(
                "execute-work",
                "POST",
                "work/{id}/execution",
                "work:execute",
            ),
            op("native-config", "GET", "native-config", "manage"),
            op("native-schema", "GET", "native-config/schema", "manage"),
            op(
                "validate-native-config",
                "POST",
                "native-config/validate",
                "manage",
            ),
            op("apply-native-config", "PUT", "native-config", "manage"),
        ]),
        "slack-adapter" => ops.extend([
            op("activity", "GET", "activity", "observe"),
            op("conversations", "GET", "conversations", "observe"),
            op("config", "GET", "config", "manage"),
            op("apply-config", "PUT", "config", "manage"),
            op("lifecycle", "POST", "lifecycle", "manage"),
            op("connection-check", "POST", "check", "manage"),
            op("permissions", "GET", "permissions", "manage"),
        ]),
        "persona-workshop" => ops.extend([
            op("projects", "GET", "projects", "observe"),
            op("activity", "GET", "activity", "observe"),
        ]),
        "generic-json" => {
            if let Ok(extra) = serde_json::from_value::<Vec<Operation>>(
                endpoint["management"]["operations"].clone(),
            ) {
                for operation in extra {
                    ops.retain(|existing| existing.id != operation.id);
                    ops.push(operation);
                }
            }
        }
        _ => return vec![],
    }
    ops
}
pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 500
        && !path.starts_with('/')
        && path.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
}
pub fn validate(value: &Value) -> ApiResult<()> {
    let label = value["label"].as_str().unwrap_or("").trim();
    if label.is_empty() || label.len() > 160 {
        return Err(ApiError::bad("Give this connection a name"));
    }
    if !["agent", "adapter", "experience", "workshop"]
        .contains(&value["kind"].as_str().unwrap_or("agent"))
    {
        return Err(ApiError::bad("Choose a supported connection kind"));
    }
    if !value["acp"].is_null() {
        crate::acp::validate(&value["acp"])?;
    }
    let m = &value["management"];
    if !m.is_null() {
        if !config::valid_origin(m["base_url"].as_str().unwrap_or("")) {
            return Err(ApiError::bad("Management address must be an HTTPS origin"));
        }
        if ![
            "agent-instance",
            "slack-adapter",
            "persona-workshop",
            "generic-json",
        ]
        .contains(&m["provider"].as_str().unwrap_or(""))
        {
            return Err(ApiError::bad("Choose a management provider"));
        }
        if !["observe", "manage"].contains(&m["access"].as_str().unwrap_or("observe")) {
            return Err(ApiError::bad("Choose observation or management access"));
        }
        if !config::safe_name(m["credential_ref"].as_str().unwrap_or(""))
            || m["credential_ref"] == "control-token"
        {
            return Err(ApiError::bad("Store a named management credential first"));
        }
        if let Some(path) = m["ca_file"].as_str()
            && !std::path::Path::new(path).is_absolute()
        {
            return Err(ApiError::bad("CA files require an absolute local path"));
        }
        let ops = operations(value);
        if ops.len() > 60
            || ops.iter().any(|o| {
                !config::safe_name(&o.id)
                    || !(valid_path(&o.path)
                        || (m["provider"] == "agent-instance"
                            && valid_path(&o.path.replace("{id}", "item"))))
                    || !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&o.method.as_str())
                    || ![
                        "observe",
                        "manage",
                        "work",
                        "work:execute",
                        "memory",
                        "memory:read",
                    ]
                    .contains(&o.access.as_str())
                    || (["observe", "work", "memory:read"].contains(&o.access.as_str())
                        && o.method != "GET")
                    || (["descriptor", "status"].contains(&o.id.as_str())
                        && (o.method != "GET" || o.access != "observe"))
            })
        {
            return Err(ApiError::bad("Invalid management provider operation"));
        }
    }
    if value["acp"].is_null() && m.is_null() && value["ui_url"].as_str().is_none() {
        return Err(ApiError::bad(
            "Add an ACP endpoint, management connection, or experience address",
        ));
    }
    if let Some(url) = value["ui_url"].as_str()
        && !config::valid_origin(url)
    {
        return Err(ApiError::bad("Experience address must be an HTTPS origin"));
    }
    Ok(())
}
pub fn client(state: &AppState, endpoint: &Value) -> ApiResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none());
    if let Some(path) = endpoint["management"]["ca_file"].as_str() {
        let bytes = std::fs::read(path)?;
        let cert = reqwest::Certificate::from_pem(&bytes)
            .map_err(|_| ApiError::bad("Invalid trust certificate"))?;
        builder = builder.add_root_certificate(cert);
    }
    let _ = state;
    builder
        .build()
        .map_err(|_| ApiError::bad("Could not prepare management connection"))
}
async fn request(
    state: &AppState,
    endpoint: &Value,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> ApiResult<Value> {
    if !valid_path(path) {
        return Err(ApiError::bad("Invalid provider path"));
    }
    let m = &endpoint["management"];
    let origin = m["base_url"]
        .as_str()
        .ok_or_else(|| ApiError::bad("No management provider attached"))?;
    let token = state.secret(m["credential_ref"].as_str().unwrap_or(""))?;
    let mut req = client(state, endpoint)?
        .request(
            method
                .parse()
                .map_err(|_| ApiError::bad("Invalid method"))?,
            format!("{}/api/v1/{path}", origin.trim_end_matches('/')),
        )
        .bearer_auth(token);
    if let Some(v) = body {
        req = req.json(v);
    }
    let mut response=req.send().await.map_err(|_|ApiError(axum::http::StatusCode::BAD_GATEWAY,"The management endpoint could not be reached with its configured trust and credentials".into()))?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ApiError::bad("Management response interrupted"))?
    {
        if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
            return Err(ApiError::bad("Management response too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(ApiError(
            if status.as_u16() == 404 {
                axum::http::StatusCode::NOT_IMPLEMENTED
            } else {
                axum::http::StatusCode::BAD_GATEWAY
            },
            format!(
                "Management provider returned {status}; inspect its permissions and supported features"
            ),
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::bad("Management provider did not return JSON"))
}
pub fn identity(descriptor: &Value) -> Option<&str> {
    descriptor["instance_id"]
        .as_str()
        .or_else(|| descriptor["id"].as_str())
}
async fn identity_document(state: &AppState, endpoint: &Value) -> ApiResult<Value> {
    let operation = if endpoint["management"]["provider"] == "persona-workshop" {
        "status"
    } else {
        "descriptor"
    };
    let path = operations(endpoint)
        .into_iter()
        .find(|o| o.id == operation)
        .ok_or_else(|| ApiError::bad("This provider needs an identity observation"))?
        .path;
    request(state, endpoint, "GET", &path, None).await
}
pub fn available_operations(endpoint: &Value, descriptor: &Value) -> Vec<Operation> {
    let all = operations(endpoint);
    if endpoint["management"]["provider"] != "agent-instance" {
        return all;
    }
    let Some(advertised) = descriptor["operations"].as_array() else {
        return all
            .into_iter()
            .filter(|o| !optional_operation(&o.id))
            .collect();
    };
    all.into_iter()
        .filter(|op| {
            ["descriptor", "status"].contains(&op.id.as_str())
                || advertised.iter().any(|a| {
                    a["path"] == op.path
                        && a["methods"]
                            .as_array()
                            .is_some_and(|methods| methods.iter().any(|m| m == &op.method))
                })
        })
        .collect()
}
fn optional_operation(id: &str) -> bool {
    [
        "updates",
        "request-update",
        "update-policy",
        "memory",
        "remember",
        "memory-item",
        "memory-command",
        "scheduler",
        "configure-scheduler",
        "work-item",
        "work-command",
        "work-execution",
        "execute-work",
        "native-config",
        "native-schema",
        "validate-native-config",
        "apply-native-config",
    ]
    .contains(&id)
}
/// Resource identifiers are data, never an arbitrary provider URL or path.
pub fn operation_request(
    selected: &Operation,
    body: Option<&Value>,
) -> ApiResult<(String, Option<Value>)> {
    if !selected.path.contains("{id}") {
        return Ok((selected.path.clone(), body.cloned()));
    }
    let body = body.ok_or_else(|| ApiError::bad("Select a resource"))?;
    let id = body["resource_id"]
        .as_str()
        .ok_or_else(|| ApiError::bad("Select a resource"))?;
    uuid::Uuid::parse_str(id).map_err(|_| ApiError::bad("Invalid resource identifier"))?;
    if body
        .as_object()
        .is_none_or(|o| o.keys().any(|k| k != "resource_id" && k != "payload"))
    {
        return Err(ApiError::bad("Use resource_id and an optional payload"));
    }
    Ok((
        selected.path.replace("{id}", id),
        body.get("payload").cloned(),
    ))
}
pub async fn call(
    state: &AppState,
    endpoint: &Value,
    operation: &str,
    body: Option<&Value>,
) -> ApiResult<Value> {
    let selected = operations(endpoint)
        .into_iter()
        .find(|o| o.id == operation)
        .ok_or_else(|| {
            ApiError(
                axum::http::StatusCode::NOT_IMPLEMENTED,
                "This provider does not offer that operation".into(),
            )
        })?;
    if (selected.access == "manage" || selected.method != "GET")
        && endpoint["management"]["access"] != "manage"
    {
        return Err(ApiError::bad("This connection has observation access only"));
    }
    if selected.method != "GET" || !["descriptor", "status"].contains(&operation) {
        let expected = endpoint["management"]["expected_id"].as_str();
        if selected.access != "observe" && expected.is_none() {
            return Err(ApiError::bad(
                "Inspect and pin this installation identity before accessing its private controls",
            ));
        }
        if let Some(expected) = expected {
            let descriptor = identity_document(state, endpoint).await?;
            if identity(&descriptor) != Some(expected) {
                return Err(ApiError::bad(
                    "The remote installation identity changed; reconnect deliberately",
                ));
            }
            if endpoint["management"]["provider"] == "agent-instance"
                && optional_operation(operation)
                && !available_operations(endpoint, &descriptor)
                    .iter()
                    .any(|o| o.id == operation)
            {
                return Err(ApiError(
                    axum::http::StatusCode::NOT_IMPLEMENTED,
                    "This installation does not advertise that optional capability".into(),
                ));
            }
        }
    }
    let (path, payload) = operation_request(&selected, body)?;
    request(state, endpoint, &selected.method, &path, payload.as_ref()).await
}
pub async fn inspect(state: &AppState, endpoint: &Value) -> ApiResult<Value> {
    if endpoint["management"].is_null() {
        return Ok(
            json!({"endpoint_id":endpoint["id"],"observed_at":now_ms(),"management":"unsupported","health":"unknown","note":"ACP connectivity can be checked independently. This agent does not need a management provider."}),
        );
    }
    let descriptor = call(state, endpoint, "descriptor", None).await?;
    let status = call(state, endpoint, "status", None).await?;
    let actual_id = if endpoint["management"]["provider"] == "persona-workshop" {
        identity(&status)
    } else {
        identity(&descriptor)
    };
    if let Some(expected) = endpoint["management"]["expected_id"].as_str()
        && actual_id != Some(expected)
    {
        return Err(ApiError::bad("The remote installation identity changed"));
    }
    Ok(
        json!({"id":endpoint["id"],"endpoint_id":endpoint["id"],"instance_id":actual_id,"observed_at":now_ms(),"management":"available","descriptor":descriptor,"status":status,"operations":available_operations(endpoint,&descriptor),"health":"reachable"}),
    )
}
