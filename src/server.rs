use crate::{
    acp, auth, config,
    error::{ApiError, ApiResult},
    provider, provision,
    state::{AppState, now_ms},
    views,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn router(s: Arc<AppState>) -> Router {
    Router::new()
        .merge(crate::machines::routes())
        .route(
            "/markdown-notices.txt",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../ui/MARKDOWN-NOTICES.txt"),
                )
            }),
        )
        .route(
            "/markdown.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    concat!(
                        include_str!("../ui/markdown.js"),
                        include_str!("../ui/markdown-renderer.js")
                    ),
                )
            }),
        )
        .route("/", get(index))
        .route("/advanced", get(advanced))
        .route("/world.js", get(world_script))
        .route("/world-model.js", get(world_model))
        .route("/world.css", get(world_styles))
        .route("/world-assets/{name}", get(world_asset))
        .route("/api/v1/places", get(places).post(add_place))
        .route("/api/v1/places/{id}", put(edit_place).delete(delete_place))
        .route("/app.js", get(script))
        .route("/app.css", get(styles))
        .route("/agents/{id}/context/v1/query", post(crate::context::query))
        .route("/healthz", get(|| async { Json(json!({"ok":true})) }))
        .route("/auth/session", get(auth::session))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/api/v1/descriptor", get(descriptor))
        .route("/api/v1/local-proof", get(auth::local_proof))
        .route("/api/v1/status", get(status))
        .route("/api/v1/native-update/stop", post(update_stop))
        .route("/api/v1/native-update/finish", post(update_finish))
        .route("/api/v1/endpoints", get(endpoints).post(add_endpoint))
        .route(
            "/api/v1/endpoints/{id}",
            get(endpoint).put(edit_endpoint).delete(delete_endpoint),
        )
        .route("/api/v1/endpoints/{id}/inspect", post(inspect))
        .route("/api/v1/endpoints/{id}/probe", post(probe))
        .route(
            "/api/v1/endpoints/{id}/operations/{operation}",
            post(operation),
        )
        .route("/api/v1/credentials", get(credentials).post(credential))
        .route("/api/v1/hosts", get(hosts).post(add_host))
        .route("/api/v1/hosts/{id}", put(edit_host).delete(delete_host))
        .route("/api/v1/install/plan", post(plan))
        .route("/api/v1/install", post(install))
        .route("/api/v1/jobs", get(jobs))
        .route("/api/v1/jobs/{id}", get(job))
        .route("/api/v1/jobs/{id}/connect", post(connect_installation))
        .route("/api/v1/jobs/{id}/log/{stream}", get(job_log))
        .route("/api/v1/sessions", get(sessions).post(new_session))
        .route("/api/v1/sessions/{id}", get(session))
        .route("/api/v1/endpoints/{id}/acp-sessions", post(agent_sessions))
        .route("/api/v1/sessions/{id}/prompt", post(prompt))
        .route("/api/v1/sessions/{id}/inputs/resume", post(resume_inputs))
        .route("/api/v1/sessions/{id}/close", post(close_session))
        .route(
            "/api/v1/sessions/{id}/elicitations/{request}",
            post(decide_elicitation),
        )
        .route("/api/v1/sessions/{id}/cancel", post(cancel_turn))
        .route(
            "/api/v1/sessions/{id}/permissions/{permission}",
            post(decide_permission),
        )
        .route("/api/v1/links", get(links).post(add_link))
        .route("/api/v1/links/{id}", put(edit_link).delete(delete_link))
        .route("/api/v1/experiments", get(experiments).post(add_experiment))
        .route(
            "/api/v1/experiments/{id}/transcript",
            get(crate::experiments::read).put(crate::experiments::upload),
        )
        .route("/api/v1/activity", get(activity))
        .route("/api/v1/tokens", post(token))
        .route("/api/v1/tokens/{id}", axum::routing::delete(revoke_token))
        .route("/api/v1/views", post(new_view))
        .route("/api/v1/views/{id}", get(view))
        .route("/api/v1/views/{id}/commands", post(view_command))
        .route("/api/v1/views/{id}/ack", post(view_ack))
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(s.clone(), headers))
        .with_state(s)
}
async fn index() -> Html<&'static str> {
    Html(include_str!("../ui/world-index.html"))
}
async fn advanced() -> Html<&'static str> {
    Html(include_str!("../ui/index.html"))
}
async fn world_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../ui/world.js"),
    )
}
async fn world_model() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../ui/world-model.js"),
    )
}
async fn world_styles() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../ui/world.css"),
    )
}
async fn world_asset(Path(name): Path<String>) -> Response {
    let (mime, bytes): (&str, &[u8]) = match name.as_str() {
        "frame.webp" => (
            "image/webp",
            include_bytes!("../ui/world-assets/frame.webp"),
        ),
        "places.webp" => (
            "image/webp",
            include_bytes!("../ui/world-assets/places.webp"),
        ),
        "robots.webp" => (
            "image/webp",
            include_bytes!("../ui/world-assets/robots.webp"),
        ),
        "maker.webp" => (
            "image/webp",
            include_bytes!("../ui/world-assets/maker.webp"),
        ),
        "coach.webp" => (
            "image/webp",
            include_bytes!("../ui/world-assets/coach.webp"),
        ),
        "rook.webp" => ("image/webp", include_bytes!("../ui/world-assets/rook.webp")),
        "club.webp" => ("image/webp", include_bytes!("../ui/world-assets/club.webp")),
        "voxel.webp" => (
            "image/webp",
            include_bytes!("../ui/world-assets/voxel.webp"),
        ),
        "slack.svg" => (
            "image/svg+xml",
            include_bytes!("../ui/world-assets/slack.svg"),
        ),
        "teams.svg" => (
            "image/svg+xml",
            include_bytes!("../ui/world-assets/teams.svg"),
        ),
        "nunito.woff2" => (
            "font/woff2",
            include_bytes!("../ui/world-assets/nunito.woff2"),
        ),
        "NUNITO-LICENSE.txt" => (
            "text/plain",
            include_bytes!("../ui/world-assets/NUNITO-LICENSE.txt"),
        ),
        _ => return axum::http::StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, mime)], bytes).into_response()
}
async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../ui/app.js"),
    )
}
async fn styles() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        concat!(
            include_str!("../ui/app.css"),
            "\n",
            include_str!("../ui/markdown.css")
        ),
    )
}
async fn headers(State(s): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    if !req.headers().contains_key(header::HOST)
        && let Some(authority) = req.uri().authority().map(|a| a.as_str().to_owned())
        && let Ok(value) = authority.parse()
    {
        req.headers_mut().insert(header::HOST, value);
    }
    if s.config.web_transport == config::WebTransport::LocalHttp {
        let expected_host = s.config.public_url.strip_prefix("http://").unwrap_or("");
        let actual_host = req
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok());
        let foreign_origin = req
            .headers()
            .get(header::ORIGIN)
            .is_some_and(|h| h.to_str().ok() != Some(s.config.public_url.as_str()));
        let foreign_site = req
            .headers()
            .get("sec-fetch-site")
            .is_some_and(|h| !matches!(h.to_str().ok(), Some("same-origin" | "none")));
        if actual_host != Some(expected_host) || foreign_origin || foreign_site {
            return ApiError::forbidden().into_response();
        }
    }
    if crate::desktop_update::busy(&s)
        && req.method() != axum::http::Method::GET
        && !req.uri().path().starts_with("/api/v1/native-update/")
    {
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":"Observatory is reopening after its local update. Your agents keep running."}))).into_response();
    }
    let mut response = next.run(req).await;
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    h.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    let ancestors = if s.config.embed_origins.is_empty() {
        "'none'".into()
    } else {
        s.config.embed_origins.join(" ")
    };
    h.insert(header::CONTENT_SECURITY_POLICY,format!("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; media-src 'self' data:; connect-src 'self'; frame-src https:; frame-ancestors {ancestors}; base-uri 'self'; form-action 'self'").parse().unwrap());
    response
}
async fn update_stop(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    Ok(Json(crate::desktop_update::stop(&s, &body)?))
}
async fn update_finish(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    Ok(Json(crate::desktop_update::finish(&s, &body)?))
}
async fn descriptor(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "observe", false)?;
    Ok(Json(
        json!({"id":s.config.id,"name":"Agent Observatory","version":env!("CARGO_PKG_VERSION"),"contract":"agent-observatory/1","boundary":"ACP with optional independent management providers","capabilities":{"endpoints":true,"management_providers":true,"host_installation":true,"machine_pairing":s.config.web_transport != config::WebTransport::LocalHttp,"stdio_acp":true,"semantic_views":true,"work_observation":true,"experiment_reports":true,"experiment_transcripts":true},"scopes":["observe","work","work:execute","memory:read","memory","manage","interact","provision","ui","admin"]}),
    ))
}
async fn status(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "observe", false)?;
    Ok(Json(
        json!({"id":s.config.id,"version":env!("CARGO_PKG_VERSION"),"endpoints":s.list("endpoints")?.len(),"hosts":s.list("hosts")?.len(),"jobs":s.list("jobs")?.len(),"active_sessions":s.active_sessions.lock().unwrap().len(),"pid":std::process::id(),"binary_sha256":crate::desktop::binary_digest().map_err(ApiError::from)?}),
    ))
}
fn revision(body: &Value) -> ApiResult<u64> {
    body["expected_revision"]
        .as_u64()
        .ok_or_else(|| ApiError::bad("Refresh this record before saving"))
}
fn allowed(body: &Value, fields: &[&str]) -> ApiResult<Value> {
    let obj = body
        .as_object()
        .ok_or_else(|| ApiError::bad("Expected an object"))?;
    if obj
        .keys()
        .any(|k| !fields.contains(&k.as_str()) && k != "expected_revision")
    {
        return Err(ApiError::bad("Unknown configuration field"));
    }
    Ok(Value::Object(
        obj.iter()
            .filter(|(k, _)| k.as_str() != "expected_revision")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    ))
}
fn endpoint_body(body: &Value) -> ApiResult<Value> {
    let v = allowed(
        body,
        &[
            "label",
            "kind",
            "acp",
            "management",
            "ui_url",
            "notes",
            "host_id",
        ],
    )?;
    provider::validate(&v)?;
    Ok(v)
}
fn validate_host_reference(s: &AppState, e: &Value) -> ApiResult<()> {
    if let Some(id) = e["host_id"].as_str() {
        s.read("hosts", id)
            .map_err(|_| ApiError::bad("Choose a saved computer before assigning this agent"))?;
    }
    Ok(())
}
async fn endpoints(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "observe", false)?;
    let rows = s
        .list("endpoints")?
        .into_iter()
        .map(|e| with_observations(&s, e))
        .collect::<Vec<_>>();
    Ok(Json(json!({"endpoints":rows})))
}
fn with_observations(s: &AppState, mut e: Value) -> Value {
    let id = e["id"].as_str().unwrap();
    let management = s.read("observations", id).unwrap_or(Value::Null);
    let acp = s.read("acp_observations", id).unwrap_or(Value::Null);
    // A completed probe describes exactly the connection revision it checked.
    // An edit during or after that probe must not inherit its successful status.
    e["acp_observation"] = if acp["endpoint_revision"] == e["revision"] {
        acp
    } else {
        Value::Null
    };
    e["observation"] = management;
    e
}
async fn endpoint(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "observe", false)?;
    Ok(Json(with_observations(&s, s.read("endpoints", &id)?)))
}
async fn add_endpoint(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "manage", true)?;
    let _inventory = s.inventory_registry_lock.lock().unwrap();
    let mut e = endpoint_body(&body)?;
    validate_host_reference(&s, &e)?;
    e["id"] = json!(uuid::Uuid::new_v4());
    let e = s.save("endpoints", e, Some(0))?;
    s.audit(
        "endpoint.connected",
        e["id"].as_str(),
        json!({"actor":p.actor,"label":e["label"]}),
    )?;
    Ok(Json(e))
}
async fn edit_endpoint(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "manage", true)?;
    let _inventory = s.inventory_registry_lock.lock().unwrap();
    let previous = s.read("endpoints", &id)?;
    let mut e = endpoint_body(&body)?;
    if body.get("host_id").is_none() && previous.get("host_id").is_some() {
        e["host_id"] = previous["host_id"].clone();
    }
    validate_host_reference(&s, &e)?;
    e["id"] = json!(id);
    let e = s.save("endpoints", e, Some(revision(&body)?))?;
    s.audit("endpoint.changed", Some(&id), json!({"actor":p.actor}))?;
    Ok(Json(e))
}
async fn delete_endpoint(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "manage", true)?;
    s.remove("endpoints", &id, revision(&body)?)?;
    s.audit("endpoint.detached", Some(&id), json!({"actor":p.actor}))?;
    Ok(Json(json!({"detached":true})))
}
async fn inspect(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "observe", true)?;
    let e = s.read("endpoints", &id)?;
    let result = provider::inspect(&s, &e).await;
    let value = match result {
        Ok(mut value) => {
            value["id"] = json!(id);
            value
        }
        Err(err) => {
            json!({"id":id,"endpoint_id":id,"observed_at":now_ms(),"health":"unavailable","error":err.1})
        }
    };
    s.save("observations", value.clone(), None)?;
    Ok(Json(value))
}
async fn probe(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "interact", true)?;
    let e = s.read("endpoints", &id)?;
    let _permit = s
        .operation_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::busy())?;
    let result = acp::probe(&s, &e["acp"])
        .await
        .map_err(|e| ApiError::bad(format!("ACP connection failed: {}", e.1)));
    let mut observation =
        json!({"id":id,"endpoint_id":id,"endpoint_revision":e["revision"],"observed_at":now_ms()});
    match &result {
        Ok(value) => {
            observation["health"] = json!("reachable");
            observation["protocol_version"] = value["protocolVersion"].clone();
            observation["agent_info"] = value["agentInfo"].clone();
            observation["capabilities"] = value["agentCapabilities"].clone();
        }
        Err(err) => {
            observation["health"] = json!("unavailable");
            observation["error"] = json!(err.1);
        }
    }
    s.save("acp_observations", observation.clone(), None)?;
    s.audit(
        if result.is_ok() {
            "acp.checked"
        } else {
            "acp.check_failed"
        },
        Some(&id),
        observation,
    )?;
    Ok(Json(result?))
}
async fn operation(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((id, name)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let e = s.read("endpoints", &id)?;
    let selected = provider::operations(&e)
        .into_iter()
        .find(|o| o.id == name)
        .ok_or_else(|| ApiError::bad("Operation not supported by this provider"))?;
    let scope = selected.access.as_str();
    let p = if scope == "memory:read" {
        auth::check(&s, &h, scope, true).or_else(|_| auth::check(&s, &h, "memory", true))?
    } else {
        auth::check(&s, &h, scope, true)?
    };
    let _permit = s
        .operation_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::busy())?;
    let result = provider::call(&s, &e, &name, Some(&body)).await?;
    if selected.method != "GET" {
        s.audit(
            "management.applied",
            Some(&id),
            json!({"actor":p.actor,"operation":name}),
        )?;
    }
    Ok(Json(result))
}
async fn credentials(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "manage", false)?;
    let mut names = Vec::new();
    for entry in std::fs::read_dir(s.home.join("secrets"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if config::safe_name(&name) && name != "control-token" {
            names.push(name);
        }
    }
    Ok(Json(json!({"names":names})))
}
async fn credential(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "manage", true)?;
    let name = body["name"].as_str().unwrap_or("");
    let value = body["value"].as_str().unwrap_or("");
    if !config::safe_name(name)
        || name == "control-token"
        || value.is_empty()
        || value.len() > 65536
    {
        return Err(ApiError::bad("Choose a credential name and value"));
    }
    let path = s.home.join("secrets").join(name);
    if body["create_only"] == true {
        config::private_create(&path, value.as_bytes()).map_err(|error| {
            if error.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) {
                ApiError(axum::http::StatusCode::CONFLICT, "This credential name is already in use. Choose a new name or use the existing credential without replacing it.".into())
            } else { ApiError::bad("Could not store the new credential") }
        })?;
    } else {
        config::private_write(&path, value.as_bytes())?;
    }
    s.audit(
        "credential.stored",
        None,
        json!({"actor":p.actor,"name":name}),
    )?;
    Ok(Json(json!({"name":name,"stored":true})))
}
async fn hosts(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "observe", false)?;
    Ok(Json(
        json!({"hosts":crate::machines::project_hosts(&s,s.list("hosts")?)?}),
    ))
}
fn host_body(body: &Value) -> ApiResult<Value> {
    let v = allowed(
        body,
        &["label", "transport", "address", "install_roots", "recipes"],
    )?;
    provision::validate_host(&v)?;
    Ok(v)
}
async fn add_host(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "provision", true)?;
    let mut v = host_body(&body)?;
    v["id"] = json!(uuid::Uuid::new_v4());
    let v = s.save("hosts", v, Some(0))?;
    s.audit(
        "host.authorized",
        v["id"].as_str(),
        json!({"actor":p.actor}),
    )?;
    Ok(Json(v))
}
async fn edit_host(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "provision", true)?;
    let _inventory = s.inventory_registry_lock.lock().unwrap();
    let old = s.read("hosts", &id)?;
    if old["transport"] == "paired" {
        return crate::machines::edit_label(&s, &id, &body).map(Json);
    }
    let mut v = host_body(&body)?;
    if old["native_local"] == true && v["transport"] == "local" {
        v["native_local"] = json!(true);
    }
    v["id"] = json!(id);
    let v = s.save("hosts", v, Some(revision(&body)?))?;
    s.audit("host.changed", Some(&id), json!({"actor":p.actor}))?;
    Ok(Json(v))
}
async fn delete_host(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "provision", true)?;
    let _inventory = s.inventory_registry_lock.lock().unwrap();
    crate::machines::check_removable(&s, &id)?;
    if s.list("endpoints")?
        .iter()
        .any(|e| e["host_id"].as_str() == Some(&id))
    {
        return Err(ApiError(axum::http::StatusCode::CONFLICT,"Reassign this computer's inventory entries before removing it. Agents are not deleted.".into()));
    }
    s.remove("hosts", &id, revision(&body)?)?;
    Ok(Json(json!({"removed":true})))
}
async fn plan(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "provision", true)?;
    Ok(Json(provision::plan(&s, &body)?))
}
async fn install(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "provision", true)?;
    Ok(Json(provision::start(s, &body, &p.actor)?))
}
async fn jobs(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "provision", false)?;
    Ok(Json(json!({"jobs":s.list("jobs")?})))
}
async fn job(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "provision", false)?;
    Ok(Json(s.read("jobs", &id)?))
}
async fn connect_installation(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "provision", true)?;
    let mut job = s.read("jobs", &id)?;
    if job["status"] != "completed" || job["exit_code"] != 0 {
        return Err(ApiError::bad(
            "Only a completed installation can finish connecting",
        ));
    }
    let endpoint = crate::local_install::attach(&s, &job)
        .await
        .map_err(|error| ApiError::bad(error.to_string()))?;
    job["endpoint_id"] = endpoint["id"].clone();
    job["connection_pending"] = json!(false);
    job["note"] = json!(
        "Your agent is installed and connected. Choose its model access and the places it can join."
    );
    let job = s.save("jobs", job, None)?;
    s.audit(
        "installation.connection_finished",
        Some(&id),
        json!({"actor":p.actor}),
    )?;
    Ok(Json(job))
}
async fn job_log(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((id, stream)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "provision", false)?;
    s.read("jobs", &id)?;
    if uuid::Uuid::parse_str(&id).is_err() || !["stdout", "stderr"].contains(&stream.as_str()) {
        return Err(ApiError::bad("Unknown job log"));
    }
    let path = s.home.join("jobs").join(format!("{id}.{stream}"));
    let text = std::fs::read_to_string(path).unwrap_or_default();
    Ok(Json(json!({"text":text})))
}
async fn agent_sessions(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "manage", true)?;
    let _permit = s
        .operation_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::busy())?;
    let endpoint = s.read("endpoints", &id)?;
    let action = body["action"].as_str().unwrap_or("list");
    if !["list", "delete"].contains(&action) {
        return Err(ApiError::bad("Choose list or delete"));
    }
    let delete = if action == "delete" {
        if body["confirm"] != true {
            return Err(ApiError::bad(
                "Confirm removal from the agent's history; saved client transcripts are separate",
            ));
        }
        let sid = body["session_id"]
            .as_str()
            .ok_or_else(|| ApiError::bad("Choose a session"))?;
        let active = s.active_sessions.lock().unwrap();
        if s.list("sessions")?.iter().any(|v| {
            v["endpoint_id"] == id
                && v["acp_session_id"] == sid
                && active.contains(v["id"].as_str().unwrap_or(""))
        }) {
            return Err(ApiError::busy());
        }
        Some(sid)
    } else {
        None
    };
    let result = acp::catalog(&s, &endpoint["acp"], body["cursor"].as_str(), delete).await?;
    if let Some(sid) = delete {
        s.audit(
            "agent.session_removed",
            Some(&id),
            json!({"session_id":sid}),
        )?;
        for mut local in s
            .list("sessions")?
            .into_iter()
            .filter(|v| v["endpoint_id"] == id && v["acp_session_id"] == sid)
        {
            s.acp_clients
                .lock()
                .unwrap()
                .remove(local["id"].as_str().unwrap());
            local["agent_history_removed"] = json!(true);
            s.save("sessions", local.clone(), local["revision"].as_u64())?;
        }
    }
    Ok(Json(result))
}
async fn sessions(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "interact", false)?;
    Ok(Json(json!({"sessions":s.list("sessions")?})))
}
async fn session(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "interact", false)?;
    let mut record = s.read("sessions", &id)?;
    if let Some(progress) = s.progress.lock().unwrap().get(&id) {
        record["progress"] = progress.clone();
    }
    record["permissions"] = json!(s.permissions.list(&id, &p.actor));
    record["elicitations"] = json!(s.elicitations.list(&id, &p.actor));
    record["can_cancel"] = json!(
        s.cancellations
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|(actor, _)| actor == &p.actor)
    );
    Ok(Json(record))
}
async fn new_session(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "interact", true)?;
    let endpoint = s.read("endpoints", body["endpoint_id"].as_str().unwrap_or(""))?;
    acp::validate(&endpoint["acp"])?;
    let title = body["title"].as_str().unwrap_or("New conversation");
    if title.len() > 200 {
        return Err(ApiError::bad("Use a shorter title"));
    }
    Ok(Json(s.save("sessions",json!({"id":uuid::Uuid::new_v4(),"endpoint_id":endpoint["id"],"endpoint_revision":endpoint["revision"],"title":title,"messages":[],"status":"idle","acp_session_id":null}),Some(0))?))
}
async fn close_session(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "interact", true)?;
    {
        let _gate = s.talk_gate.lock().unwrap();
        if crate::desktop_update::busy(&s) {
            return Err(ApiError::busy());
        }
        let mut session = s.read("sessions", &id)?;
        let mut active = s.active_sessions.lock().unwrap();
        if !active.insert(id.clone()) {
            return Err(ApiError::busy());
        }
        session["status"] = json!("disconnecting");
        if let Err(error) = s.save("sessions", session.clone(), session["revision"].as_u64()) {
            active.remove(&id);
            return Err(error);
        }
    }
    let closed = acp::disconnect(&s, &id).await;
    let _gate = s.talk_gate.lock().unwrap();
    s.active_sessions.lock().unwrap().remove(&id);
    let mut session = s.read("sessions", &id)?;
    session["status"] = json!("disconnected");
    let v = s.save("sessions", session.clone(), session["revision"].as_u64())?;
    closed?;
    Ok(Json(v))
}
async fn decide_elicitation(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((id, request)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "interact", true)?;
    s.read("sessions", &id)?;
    let action = body["action"].as_str().unwrap_or("");
    s.elicitations
        .resolve(&id, &p.actor, &request, action, body.get("content"))
        .map_err(|e| ApiError(axum::http::StatusCode::CONFLICT, e.into()))?;
    s.audit(
        "elicitation.decided",
        Some(&id),
        json!({"actor":p.actor,"request_id":request,"action":action}),
    )?;
    Ok(Json(json!({"accepted":true})))
}
async fn decide_permission(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((id, permission)): Path<(String, String)>,
    Json(body): Json<PermissionDecision>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "interact", true)?;
    s.read("sessions", &id)?;
    let decision_kind = s
        .permissions
        .resolve(&id, &p.actor, &permission, body.option_id.as_deref())
        .map_err(|message| ApiError(axum::http::StatusCode::CONFLICT, message.into()))?;
    s.audit(
        "permission.decided",
        Some(&id),
        json!({"actor":p.actor,"request_id":permission,"decision":decision_kind.unwrap_or_else(||"cancelled".into())}),
    )?;
    Ok(Json(json!({"accepted":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionDecision {
    option_id: Option<String>,
}
async fn cancel_turn(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "interact", true)?;
    let cancellations = s.cancellations.lock().unwrap();
    let (actor, tx) = cancellations.get(&id).ok_or_else(ApiError::missing)?;
    if actor != &p.actor {
        return Err(ApiError::forbidden());
    }
    let _ = tx.send(true);
    Ok(Json(json!({"requested":true})))
}
async fn prompt(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "interact", true)?;
    Ok(Json(crate::intake::submit(s, &id, &p, &body)?))
}
async fn resume_inputs(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "interact", true)?;
    Ok(Json(crate::intake::resume(
        s,
        &id,
        &p.actor,
        body["acknowledge_unknown"] == true,
    )?))
}

fn link_body(s: &AppState, b: &Value) -> ApiResult<Value> {
    let v = allowed(b, &["label", "from", "to", "relation", "notes"])?;
    for key in ["from", "to"] {
        s.read("endpoints", v[key].as_str().unwrap_or(""))?;
    }
    if !["connects", "uses", "observes", "coordinates"]
        .contains(&v["relation"].as_str().unwrap_or(""))
    {
        return Err(ApiError::bad("Choose a connection relationship"));
    }
    Ok(v)
}
async fn links(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "observe", false)?;
    Ok(Json(
        json!({"links":s.list("links")?,"note":"Inventory records describe connections; they do not grant either endpoint access."}),
    ))
}
async fn add_link(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "manage", true)?;
    let mut v = link_body(&s, &b)?;
    v["id"] = json!(uuid::Uuid::new_v4());
    Ok(Json(s.save("links", v, Some(0))?))
}
async fn edit_link(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "manage", true)?;
    let mut v = link_body(&s, &b)?;
    v["id"] = json!(id);
    Ok(Json(s.save("links", v, Some(revision(&b)?))?))
}
async fn delete_link(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "manage", true)?;
    s.remove("links", &id, revision(&b)?)?;
    Ok(Json(json!({"removed":true})))
}
async fn experiments(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "observe", false)?;
    let reports = s
        .list("experiments")?
        .into_iter()
        .map(|mut report| {
            let count = report["transcript"].as_array().map_or(0, Vec::len);
            report.as_object_mut().unwrap().remove("transcript");
            report["transcript_count"] = json!(count);
            report
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"experiments":reports})))
}
async fn add_experiment(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "manage", true)?;
    let mut v = allowed(
        &b,
        &[
            "label",
            "run_id",
            "status",
            "summary",
            "measurements",
            "report_url",
            "participants",
        ],
    )?;
    if v["label"]
        .as_str()
        .is_none_or(|v| v.is_empty() || v.len() > 200)
    {
        return Err(ApiError::bad("Name this experiment"));
    }
    if let Some(url) = v["report_url"].as_str() {
        let u = reqwest::Url::parse(url).map_err(|_| ApiError::bad("Invalid report URL"))?;
        if u.scheme() != "https" || !u.username().is_empty() || u.password().is_some() {
            return Err(ApiError::bad("Use an HTTPS report URL"));
        }
    }
    v["id"] = json!(uuid::Uuid::new_v4());
    v["evidence_source"] = json!("submitted_report");
    Ok(Json(s.save("experiments", v, Some(0))?))
}
#[derive(Deserialize)]
struct Page {
    limit: Option<usize>,
    before: Option<i64>,
}
async fn activity(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<Page>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "observe", false)?;
    let db = s.db.lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT seq,at,kind,resource_id,data FROM audit WHERE seq<?1 ORDER BY seq DESC LIMIT ?2",
    )?;
    let rows=stmt.query_map(params![q.before.unwrap_or(i64::MAX),q.limit.unwrap_or(100).min(500) as i64],|r|Ok(json!({"sequence":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"resource_id":r.get::<_,Option<String>>(3)?,"data":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null)})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Json(json!({"events":rows})))
}
async fn token(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    let scopes = b["scopes"]
        .as_array()
        .ok_or_else(|| ApiError::bad("Choose credential permissions"))?;
    if scopes.is_empty()
        || scopes.iter().any(|v| {
            ![
                "observe",
                "work",
                "work:execute",
                "memory:read",
                "memory",
                "manage",
                "interact",
                "provision",
                "ui",
                "admin",
            ]
            .contains(&v.as_str().unwrap_or(""))
                && !crate::context::valid_scope(&s, v.as_str().unwrap_or(""))
        })
    {
        return Err(ApiError::bad("Unknown credential permission"));
    }
    let secret = auth::random_token();
    let id = uuid::Uuid::new_v4().to_string();
    let expires = now_ms()
        + b["expires_in_seconds"]
            .as_i64()
            .unwrap_or(86400)
            .clamp(60, 90 * 86400)
            * 1000;
    s.db.lock().unwrap().execute(
        "INSERT INTO tokens VALUES(?1,?2,?3,?4)",
        params![
            auth::digest(&secret),
            id,
            serde_json::to_string(scopes).unwrap(),
            expires
        ],
    )?;
    Ok(Json(json!({"id":id,"token":secret,"expires_at":expires})))
}
async fn revoke_token(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    if id == "local-control" {
        return Err(ApiError::bad("Local control is managed on the host"));
    }
    s.db.lock()
        .unwrap()
        .execute("DELETE FROM tokens WHERE id=?1", [id])?;
    Ok(Json(json!({"revoked":true})))
}
async fn new_view(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "ui", true)?;
    Ok(Json(views::create(&s)?))
}
async fn view(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "ui", false)?;
    Ok(Json(views::read(&s, &id)?))
}
async fn view_command(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "ui", true)?;
    Ok(Json(views::commands(&s, &id, &b)?))
}
async fn view_ack(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "ui", true)?;
    Ok(Json(views::ack(
        &s,
        &id,
        b["revision"]
            .as_u64()
            .ok_or_else(|| ApiError::bad("Revision required"))?,
    )?))
}

async fn places(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "observe", false)?;
    Ok(Json(json!({"places":s.list("places")?})))
}
async fn add_place(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let principal = auth::check(&s, &h, "manage", true)?;
    let _guard = s.places_registry_lock.lock().unwrap();
    let mut value = crate::places::body(&s, &body)?;
    crate::places::validate_bindings(&s, &value, None)?;
    value["id"] = json!(uuid::Uuid::new_v4());
    let saved = s.save("places", value, Some(0))?;
    s.audit(
        "place.added",
        saved["id"].as_str(),
        json!({"actor":principal.actor}),
    )?;
    Ok(Json(saved))
}
async fn edit_place(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let principal = auth::check(&s, &h, "manage", true)?;
    let _guard = s.places_registry_lock.lock().unwrap();
    s.read("places", &id)?;
    let mut value = crate::places::body(&s, &body)?;
    crate::places::validate_bindings(&s, &value, Some(&id))?;
    value["id"] = json!(id);
    let saved = s.save("places", value, Some(revision(&body)?))?;
    s.audit("place.changed", Some(&id), json!({"actor":principal.actor}))?;
    Ok(Json(saved))
}
async fn delete_place(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let principal = auth::check(&s, &h, "manage", true)?;
    let _guard = s.places_registry_lock.lock().unwrap();
    s.remove("places", &id, revision(&body)?)?;
    s.audit("place.removed", Some(&id), json!({"actor":principal.actor}))?;
    Ok(Json(json!({"removed":true,"connections_unchanged":true})))
}
