//! Display grouping for source-owned venues. Bindings never connect an agent or grant access.
use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};
use serde_json::{Map, Value};
use std::collections::HashSet;

fn fields<'a>(value: &'a Value, allowed: &[&str]) -> ApiResult<&'a Map<String, Value>> {
    let object = value
        .as_object()
        .ok_or_else(|| ApiError::bad("Expected a place configuration object"))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ApiError::bad("Unknown place configuration field"));
    }
    Ok(object)
}

fn label(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.trim().is_empty() && s.len() <= 160 && !s.chars().any(char::is_control))
}

fn key(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 200
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
    })
}

/// Validate editable fields. Record IDs and revisions are assigned by the registry.
pub fn body(state: &AppState, input: &Value) -> ApiResult<Value> {
    let editable = fields(
        input,
        &[
            "label",
            "service",
            "kind",
            "external_id",
            "rooms",
            "bindings",
            "expected_revision",
        ],
    )?;
    if !label(&input["label"]) {
        return Err(ApiError::bad("Give this place a short name"));
    }
    if !input["service"].as_str().is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 64
            && s.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
    }) {
        return Err(ApiError::bad("Use a lowercase service identifier"));
    }
    if !matches!(
        input["kind"].as_str(),
        Some("workspace" | "server" | "experience")
    ) {
        return Err(ApiError::bad("Choose a workspace, server or experience"));
    }
    if !input["external_id"].is_null() && !key(&input["external_id"]) {
        return Err(ApiError::bad("Use an opaque external place ID"));
    }
    let rooms = input["rooms"]
        .as_array()
        .filter(|rooms| rooms.len() <= 500)
        .ok_or_else(|| ApiError::bad("Use a list of at most 500 rooms"))?;
    let mut room_ids = HashSet::new();
    for room in rooms {
        fields(room, &["id", "label"])?;
        if !key(&room["id"]) || !label(&room["label"]) {
            return Err(ApiError::bad("Each room needs an opaque ID and name"));
        }
        if !room_ids.insert(room["id"].as_str().unwrap()) {
            return Err(ApiError::bad("A room ID is repeated in this place"));
        }
    }
    let bindings = input["bindings"]
        .as_array()
        .filter(|bindings| bindings.len() <= 1000)
        .ok_or_else(|| ApiError::bad("Use a list of at most 1000 connection bindings"))?;
    let mut connection_ids = HashSet::new();
    for binding in bindings {
        fields(binding, &["endpoint_id", "connection_id", "room_id"])?;
        let endpoint = binding["endpoint_id"]
            .as_str()
            .filter(|id| uuid::Uuid::parse_str(id).is_ok())
            .ok_or_else(|| ApiError::bad("Choose an existing endpoint for this binding"))?;
        state.read("endpoints", endpoint)?;
        if !key(&binding["connection_id"]) {
            return Err(ApiError::bad("Use an opaque connection ID"));
        }
        let connection = binding["connection_id"].as_str().unwrap();
        if !connection_ids.insert((endpoint, connection)) {
            return Err(ApiError::bad(
                "A connection binding is repeated in this place",
            ));
        }
        if !binding["room_id"].is_null()
            && binding["room_id"]
                .as_str()
                .is_none_or(|id| !room_ids.contains(id))
        {
            return Err(ApiError::bad(
                "The binding's room must be declared in this place",
            ));
        }
    }
    Ok(Value::Object(
        editable
            .iter()
            .filter(|(key, _)| key.as_str() != "expected_revision")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    ))
}

/// Call after `body`, inside the same registry write lock as the following save.
/// Editing a place may retain its bindings, but cannot take another place's binding.
pub fn validate_bindings(
    state: &AppState,
    value: &Value,
    editing_id: Option<&str>,
) -> ApiResult<()> {
    let bindings = value["bindings"]
        .as_array()
        .ok_or_else(|| ApiError::bad("Place bindings must be validated before saving"))?;
    let requested: HashSet<_> = bindings
        .iter()
        .map(|binding| {
            (
                binding["endpoint_id"].as_str(),
                binding["connection_id"].as_str(),
            )
        })
        .collect();
    for place in state.list("places")? {
        if editing_id.is_some() && place["id"].as_str() == editing_id {
            continue;
        }
        let existing = place["bindings"].as_array().ok_or_else(|| {
            ApiError::bad("A stored place has invalid bindings; inspect it before saving")
        })?;
        if existing.iter().any(|binding| {
            requested.contains(&(
                binding["endpoint_id"].as_str(),
                binding["connection_id"].as_str(),
            ))
        }) {
            return Err(ApiError(
                axum::http::StatusCode::CONFLICT,
                "This endpoint connection is already grouped in another place".into(),
            ));
        }
    }
    Ok(())
}
