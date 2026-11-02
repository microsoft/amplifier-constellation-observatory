use crate::{
    auth,
    error::{ApiError, ApiResult},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Message {
    id: String,
    speaker: String,
    kind: String,
    room: String,
    text: String,
    #[serde(default)]
    at: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Upload {
    expected_revision: u64,
    messages: Vec<Message>,
}

pub async fn read(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "observe", false)?;
    let report = s.read("experiments", &id)?;
    Ok(Json(
        json!({"id":id,"revision":report["revision"],"messages":report["transcript"].as_array().cloned().unwrap_or_default()}),
    ))
}

pub async fn upload(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let actor = auth::check(&s, &h, "manage", true)?.actor;
    let input: Upload = serde_json::from_value(body)
        .map_err(|_| ApiError::bad("Use a revision and public transcript messages only"))?;
    if input.messages.len() > 500 {
        return Err(ApiError::bad("A transcript can contain up to 500 messages"));
    }
    let mut ids = HashSet::new();
    let mut total = 0usize;
    for message in &input.messages {
        total += message.text.len();
        if message.id.is_empty()
            || message.id.len() > 200
            || !ids.insert(&message.id)
            || message.speaker.trim().is_empty()
            || message.speaker.len() > 200
            || message.room.len() > 200
            || message.text.len() > 128_000
            || total > 1_000_000
            || message.at.as_ref().is_some_and(|t| t.len() > 80)
            || ![
                "simulated_human",
                "support_agent",
                "human",
                "operator_cue",
                "membership",
                "protocol_fixture",
            ]
            .contains(&message.kind.as_str())
        {
            return Err(ApiError::bad(
                "Invalid, duplicate, private, or oversized transcript entry",
            ));
        }
    }
    let mut report = s.read("experiments", &id)?;
    report["transcript"] = serde_json::to_value(input.messages).unwrap();
    let saved = s.save("experiments", report, Some(input.expected_revision))?;
    let count = saved["transcript"].as_array().map_or(0, Vec::len);
    s.audit(
        "experiment.transcript_updated",
        Some(&id),
        json!({"actor":actor,"message_count":count}),
    )?;
    Ok(Json(
        json!({"id":id,"revision":saved["revision"],"message_count":count}),
    ))
}
