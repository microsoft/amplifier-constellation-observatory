//! Optional conversation-source interface. ACP connections remain ordinary ACP.
use crate::{
    auth,
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, header},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    operation: String,
    access: Access,
    conversation_id: Option<String>,
    thread_id: Option<String>,
    limit: Option<usize>,
    before: Option<i64>,
    query: Option<String>,
}
#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum Access {
    Owner,
}

pub fn valid_scope(state: &AppState, scope: &str) -> bool {
    scope.strip_prefix("context:").is_some_and(|id| {
        uuid::Uuid::parse_str(id).is_ok()
            && state
                .read("endpoints", id)
                .is_ok_and(|e| e["acp"].is_object())
    })
}
fn source_url(state: &AppState, endpoint: &str, session: &str) -> String {
    format!(
        "{}/?agent={endpoint}&conversation={session}",
        state.config.public_url.trim_end_matches('/')
    )
}
pub async fn query(
    State(state): State<Arc<AppState>>,
    Path(endpoint_id): Path<String>,
    headers: HeaderMap,
    Json(q): Json<Query>,
) -> ApiResult<Json<Value>> {
    if !headers.contains_key(header::AUTHORIZATION) {
        return Err(ApiError::unauthorized());
    }
    auth::check(&state, &headers, &format!("context:{endpoint_id}"), false)?;
    let endpoint = state.read("endpoints", &endpoint_id)?;
    if !endpoint["acp"].is_object() {
        return Err(ApiError::forbidden());
    }
    let Access::Owner = q.access;
    if q.thread_id.is_some() {
        return Err(ApiError::bad(
            "Fleet conversations do not contain separate threads",
        ));
    }
    let search = q.query.unwrap_or_default();
    if search.len() > 200 {
        return Err(ApiError::bad("Search is too long"));
    }
    let limit = q.limit.unwrap_or(40).clamp(1, 100);
    let provider_id = format!("fleet:{}", state.config.id);
    let result = match q.operation.as_str() {
        "list" => {
            let rows: Vec<_> = state
                .list("sessions")?
                .into_iter()
                .filter(|s| {
                    s["endpoint_id"] == endpoint_id
                        && s["title"]
                            .as_str()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&search.to_lowercase())
                })
                .collect();
            let complete = rows.len() <= limit;
            let items: Vec<_> = rows.into_iter().take(limit).map(|s| {
                let id = s["id"].as_str().unwrap_or_default();
                json!({"id":id,"conversation_id":id,"label":s["title"],"surface":"observatory","updated_at":s["updated_at"],"url":source_url(&state,&endpoint_id,id),"coverage":"Messages exchanged with this agent through Observatory Talk"})
            }).collect();
            json!({"items":items,"complete":complete})
        }
        "read" | "search" | "orientation" => {
            let id = q
                .conversation_id
                .ok_or_else(|| ApiError::bad("Select a conversation"))?;
            let session = state.read("sessions", &id)?;
            if session["endpoint_id"] != endpoint_id {
                return Err(ApiError::forbidden());
            }
            let all = session["messages"].as_array().cloned().unwrap_or_default();
            let mut messages = Vec::new();
            let mut budget = 60_000usize;
            let mut complete = true;
            for (index, m) in all.iter().enumerate().rev().filter(|(_, m)| {
                q.before
                    .is_none_or(|before| m["at"].as_i64().unwrap_or(0) < before)
                    && m["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&search.to_lowercase())
            }) {
                if messages.len() == limit || budget == 0 {
                    complete = false;
                    break;
                }
                let text = m["text"].as_str().unwrap_or("");
                let mut bytes = text.len().min(budget);
                while !text.is_char_boundary(bytes) {
                    bytes -= 1;
                }
                let truncated = bytes < text.len();
                complete &= !truncated;
                budget -= bytes;
                messages.push(json!({"id":format!("{id}:{index}"),"actor":if m["role"]=="user"{json!(state.config.owner)}else{endpoint["label"].clone()},"role":m["role"],"text":&text[..bytes],"truncated":truncated,"at":m["at"],"url":source_url(&state,&endpoint_id,&id)}));
            }
            messages.reverse();
            json!({"conversation_id":id,"label":session["title"],"surface":"observatory","messages":messages,"complete":complete,"url":source_url(&state,&endpoint_id,&id),"coverage":"Saved messages in this fleet conversation; backend-only working history is not included"})
        }
        _ => {
            return Err(ApiError::bad(
                "Supported operations: list, read, search, orientation",
            ));
        }
    };
    state.audit(
        "context.queried",
        Some(&endpoint_id),
        json!({"operation":q.operation,"complete":result["complete"]}),
    )?;
    let mut result = result;
    result["provider_id"] = json!(provider_id);
    result["observed_at"] = json!(now_ms());
    Ok(Json(result))
}
