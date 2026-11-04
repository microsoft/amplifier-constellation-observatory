use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
pub fn create(s: &AppState) -> ApiResult<Value> {
    let v = json!({"id":uuid::Uuid::new_v4().to_string(),"revision":1,"screen":"agents","selected_id":null,"session_id":null,"tab":"overview","filter":"","modal":null,"draft":{},"focus":null,"scroll":null,"controller":"human","rendered_revision":0});
    s.db.lock().unwrap().execute(
        "INSERT INTO views VALUES(?1,?2)",
        params![v["id"].as_str(), v.to_string()],
    )?;
    Ok(v)
}
pub fn read(s: &AppState, id: &str) -> ApiResult<Value> {
    let raw: Option<String> =
        s.db.lock()
            .unwrap()
            .query_row("SELECT value FROM views WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
    serde_json::from_str(&raw.ok_or_else(ApiError::missing)?)
        .map_err(|_| ApiError::bad("Invalid view"))
}
fn text(v: &Value, n: usize) -> bool {
    v.as_str().is_some_and(|s| s.len() <= n)
}
fn valid(key: &str, v: &Value) -> bool {
    match key {
        "screen" => v.as_str().is_some_and(|s| {
            [
                "agents",
                "connections",
                "hosts",
                "work",
                "experiments",
                "activity",
                "settings",
            ]
            .contains(&s)
        }),
        "selected_id" | "session_id" | "permission_details" | "experiment_id" => {
            v.is_null() || v.as_str().is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
        }
        "experiment_measurements" | "agent_history_open" => v.is_boolean(),
        "agent_history_cursor" => v.is_null() || text(v, 8192),
        "agent_history_delete" => v.is_null() || text(v, 1024),
        "elicitation_drafts" => v.is_object() && v.to_string().len() <= 65536,
        "tab" => v.as_str().is_some_and(|s| {
            [
                "overview",
                "updates",
                "sessions",
                "memory",
                "background",
                "work",
                "configuration",
                "inspect",
                "chat",
                "embedded",
            ]
            .contains(&s)
        }),
        "filter" | "focus" => v.is_null() || text(v, 300),
        "modal" => {
            v.is_null()
                || v.as_str().is_some_and(|s| {
                    [
                        "connect",
                        "install",
                        "host",
                        "credentials",
                        "edit",
                        "operation",
                        "job",
                    ]
                    .contains(&s)
                })
        }
        "scroll" => {
            v.is_null()
                || v.as_object().is_some_and(|m| {
                    m.len() == 2
                        && ["x", "y"].iter().all(|k| {
                            m.get(*k)
                                .and_then(Value::as_u64)
                                .is_some_and(|n| n <= 10_000_000)
                        })
                })
        }
        "draft" => v.as_object().is_some_and(|m| {
            m.len() <= 25
                && m.iter().all(|(k, v)| match k.as_str() {
                    "label" | "kind" | "address" | "provider" | "access" | "command" | "cwd"
                    | "host_id" | "recipe_id" | "name" | "home" | "version" | "credential_ref"
                    | "ui_url" | "job_id" | "embedded_route" | "embedded_work"
                    | "memory_details" | "run_report" => text(v, 4096),
                    "args" | "configuration" | "prompt" | "host_config" | "operation_body" => {
                        text(v, 32000)
                    }
                    "advanced" => v.is_boolean(),
                    "port" => v.as_u64().is_some_and(|n| n <= 65535),
                    _ => false,
                })
        }),
        _ => false,
    }
}
pub fn commands(s: &AppState, id: &str, body: &Value) -> ApiResult<Value> {
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    let raw: Option<String> = tx
        .query_row("SELECT value FROM views WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    let mut v: Value = serde_json::from_str(&raw.ok_or_else(ApiError::missing)?).unwrap();
    if v["revision"] != body["expected_revision"] {
        return Err(ApiError(
            axum::http::StatusCode::CONFLICT,
            "The visible view changed; read its current revision".into(),
        ));
    }
    let commands = body["commands"]
        .as_array()
        .ok_or_else(|| ApiError::bad("Use semantic commands"))?;
    if commands.len() > 30 {
        return Err(ApiError::bad("Too many commands"));
    }
    let previous_selection = v["selected_id"].clone();
    for c in commands {
        let k = c["field"].as_str().unwrap_or("");
        if c["op"] != "set" || !valid(k, &c["value"]) {
            return Err(ApiError::bad(
                "Invalid view command; credentials never belong in view state",
            ));
        }
        v[k] = c["value"].clone();
    }
    // Navigating to another individual does not carry over the prior individual's chat.
    if v["selected_id"] != previous_selection {
        v["agent_history_cursor"] = Value::Null;
        v["agent_history_delete"] = Value::Null;
    }
    if v["selected_id"] != previous_selection
        && !commands.iter().any(|c| c["field"] == "session_id")
    {
        v["session_id"] = Value::Null;
    }
    if let Some(session_id) = v["session_id"].as_str() {
        let raw: Option<String> = tx
            .query_row(
                "SELECT value FROM records WHERE kind='sessions' AND id=?1",
                [session_id],
                |r| r.get(0),
            )
            .optional()?;
        let session: Value = serde_json::from_str(
            &raw.ok_or_else(|| ApiError::bad("This conversation no longer exists"))?,
        )
        .map_err(|_| ApiError::bad("Invalid stored conversation"))?;
        if v["selected_id"].is_null() || session["endpoint_id"] != v["selected_id"] {
            return Err(ApiError::bad(
                "This conversation belongs to a different agent",
            ));
        }
    }
    let controller = body["controller"].as_str().unwrap_or("programmatic");
    if controller.len() > 100 {
        return Err(ApiError::bad("Controller label too long"));
    }
    v["controller"] = json!(controller);
    v["revision"] = json!(v["revision"].as_u64().unwrap() + 1);
    tx.execute(
        "UPDATE views SET value=?2 WHERE id=?1",
        params![id, v.to_string()],
    )?;
    tx.commit()?;
    Ok(v)
}
pub fn ack(s: &AppState, id: &str, n: u64) -> ApiResult<Value> {
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    let raw: Option<String> = tx
        .query_row("SELECT value FROM views WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    let mut v: Value = serde_json::from_str(&raw.ok_or_else(ApiError::missing)?).unwrap();
    if n > v["revision"].as_u64().unwrap() {
        return Err(ApiError::bad("Cannot acknowledge a future revision"));
    }
    v["rendered_revision"] = json!(n.max(v["rendered_revision"].as_u64().unwrap_or(0)));
    tx.execute(
        "UPDATE views SET value=?2 WHERE id=?1",
        params![id, v.to_string()],
    )?;
    tx.commit()?;
    Ok(json!({"ok":true}))
}
