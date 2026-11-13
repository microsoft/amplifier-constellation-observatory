use crate::{
    client::{Bootstrap, Pending, Receipt, Remote},
    platform,
    storage::{Store, random_secret},
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Saved {
    pub schema: u32,
    pub machine_id: Uuid,
    pub receipt: Option<Receipt>,
    pub pending: Option<Pending>,
    pub disconnected: bool,
    pub revoke_pending: bool,
    pub startup_enabled: bool,
    #[serde(default)]
    pub access_refused: bool,
}
#[derive(Clone)]
struct Observation {
    state: String,
    message: String,
    last_seen: Option<u64>,
    startup_error: Option<String>,
}
pub struct App {
    pub store: Store,
    pub token: String,
    pub origin: String,
    secret: String,
    bootstrap: Mutex<Bootstrap>,
    saved: Mutex<Saved>,
    observation: Mutex<Observation>,
    operation: Mutex<()>,
    wake: Notify,
}
impl App {
    pub fn open(store: Store, bootstrap: Bootstrap, origin: String) -> Result<Self> {
        bootstrap.validate()?;
        let saved = store.read::<Saved>("state.json")?.unwrap_or(Saved {
            schema: 1,
            machine_id: Uuid::new_v4(),
            receipt: None,
            pending: None,
            disconnected: false,
            revoke_pending: false,
            startup_enabled: false,
            access_refused: false,
        });
        ensure!(saved.schema == 1, "Unsupported saved helper state");
        if let Some(old) = store.read::<Bootstrap>("observatory.json")? {
            ensure!(
                old.observatory_id == bootstrap.observatory_id
                    && old.observatory_url == bootstrap.observatory_url
                    && old.ca_pem == bootstrap.ca_pem,
                "This helper folder belongs to a different Observatory"
            );
        }
        let bootstrap = if saved.pending.is_some()
            || (saved.receipt.is_some() && !saved.disconnected && !saved.access_refused)
        {
            store
                .read::<Bootstrap>("observatory.json")?
                .unwrap_or(bootstrap)
        } else {
            bootstrap
        };
        if let Some(receipt) = &saved.receipt {
            ensure!(
                receipt.machine_id == saved.machine_id
                    && receipt.observatory_id == bootstrap.observatory_id,
                "Saved pairing identity needs review; it was not replaced"
            );
        }
        let secret = store.secret()?;
        store.save("state.json", &saved)?;
        if store.read::<Bootstrap>("observatory.json")?.is_none() {
            store.save("observatory.json", &bootstrap)?;
        }
        let state = if saved.revoke_pending {
            "disconnect_unknown"
        } else if saved.pending.is_some() {
            "unknown"
        } else if saved.access_refused {
            "access_refused"
        } else if saved.disconnected {
            "disconnected"
        } else if saved.receipt.is_some() {
            "connecting"
        } else {
            "unpaired"
        };
        Ok(Self {
            store,
            token: random_secret(),
            origin,
            secret,
            bootstrap: Mutex::new(bootstrap),
            saved: Mutex::new(saved),
            observation: Mutex::new(Observation {
                state: state.into(),
                message: String::new(),
                last_seen: None,
                startup_error: None,
            }),
            operation: Mutex::new(()),
            wake: Notify::new(),
        })
    }
    async fn status(&self) -> Value {
        let saved = self.saved.lock().await;
        let observation = self.observation.lock().await;
        let bootstrap = self.bootstrap.lock().await;
        let can_pair = !saved.revoke_pending
            && saved.pending.is_none()
            && (saved.receipt.is_none()
                || ((saved.disconnected || saved.access_refused)
                    && saved
                        .receipt
                        .as_ref()
                        .is_some_and(|r| r.pairing_id != bootstrap.pairing_id)));
        json!({"can_pair":can_pair,"observatory_name":bootstrap.observatory_name,"observatory_url":bootstrap.observatory_url,"computer_name":platform::hostname(),"os":platform::os(),"arch":platform::arch(),"state":observation.state,"message":observation.message,"last_seen_at":observation.last_seen,"startup_enabled":saved.startup_enabled,"startup_error":observation.startup_error,"pending":saved.pending.is_some(),"host_id":saved.receipt.as_ref().map(|r|r.host_id),"revoke_pending":saved.revoke_pending,"helper_version":env!("CARGO_PKG_VERSION"),"install_agents_requested":bootstrap.install_agents,"install_agents_enabled":saved.receipt.as_ref().is_some_and(|r|r.access.contains(&crate::paired_protocol::INSTALL_ACCESS.to_owned()))})
    }
    async fn pair(&self, input: PairInput) -> Result<Value> {
        let _operation = self
            .operation
            .try_lock()
            .context("A pairing operation is already being checked")?;
        let mut saved = self.saved.lock().await;
        if saved.receipt.is_some()
            && !saved.disconnected
            && !saved.access_refused
            && !saved.revoke_pending
        {
            return Ok(json!({"accepted":true}));
        }
        ensure!(
            !saved.revoke_pending,
            "Confirm the pending disconnection before pairing again"
        );
        let bootstrap = self.bootstrap.lock().await.clone();
        ensure!(
            !(saved.disconnected || saved.access_refused)
                || saved
                    .receipt
                    .as_ref()
                    .is_none_or(|r| r.pairing_id != bootstrap.pairing_id)
                || saved.pending.is_some(),
            "To reconnect, download a new setup from Observatory and open it on this computer"
        );
        let remote = Remote::new(bootstrap.clone())?;
        let pending = if let Some(pending) = &saved.pending {
            pending.clone()
        } else {
            let raw = input
                .code
                .chars()
                .filter(|c| *c != '-' && !c.is_ascii_whitespace())
                .collect::<String>()
                .to_ascii_uppercase();
            ensure!(
                raw.len() == 8 && raw.bytes().all(|b| b.is_ascii_alphanumeric()),
                "Enter the eight-character code shown in Observatory"
            );
            let code = format!("{}-{}", &raw[..4], &raw[4..]);
            Pending {
                request_id: Uuid::new_v4(),
                code,
                name: platform::hostname(),
                startup: input.startup,
                install_agents: input.install_agents && bootstrap.install_agents,
            }
        };
        // Identity, secret, original code and request are durable before the first network write.
        saved.pending = Some(pending.clone());
        self.store.save("observatory.json", &bootstrap)?;
        self.store.save("state.json", &*saved)?;
        let result = remote
            .redeem(saved.machine_id, &self.secret, &pending)
            .await;
        match result {
            Ok(receipt) => {
                saved.receipt = Some(receipt);
                saved.pending = None;
                saved.disconnected = false;
                saved.access_refused = false;
                self.store.save("state.json", &*saved)?;
                let mut observation = self.observation.lock().await;
                observation.state = "connecting".into();
                observation.message =
                    "Paired. Checking the connection; no agents or model settings were changed."
                        .into();
                observation.last_seen = None;
                self.wake.notify_one();
                if pending.startup && !saved.startup_enabled {
                    match platform::install_startup(&self.store.home, &saved.machine_id) {
                        Ok(()) => {
                            saved.startup_enabled = true;
                            observation.startup_error = None;
                            self.store.save("state.json", &*saved)?;
                        }
                        Err(error) => {
                            observation.startup_error = Some(format!(
                                "Sign-in startup could not be enabled: {error}. Keep this app running or retry startup."
                            ));
                        }
                    }
                }
                Ok(json!({"accepted":true}))
            }
            Err(error) => {
                let known = error
                    .status
                    .is_some_and(|s| [401, 403, 404, 409, 410, 429].contains(&s));
                if known {
                    saved.pending = None;
                    self.store.save("state.json", &*saved)?;
                }
                let mut observation = self.observation.lock().await;
                observation.state = if known { "unpaired" } else { "unknown" }.into();
                observation.message = error.message.clone();
                Err(error.into())
            }
        }
    }
    async fn unpair(&self) -> Result<Value> {
        let _operation = self
            .operation
            .try_lock()
            .context("A connection operation is already running")?;
        let mut saved = self.saved.lock().await;
        let receipt = saved
            .receipt
            .clone()
            .context("This computer is not paired")?;
        // Stop heartbeats before revocation. A lost reply never silently restores trust.
        saved.revoke_pending = true;
        saved.disconnected = true;
        self.store.save("state.json", &*saved)?;
        let remote = Remote::new(self.bootstrap.lock().await.clone())?;
        let result = remote.revoke(&receipt, &self.secret).await;
        match result {
            Ok(()) => {}
            Err(error) => {
                let mut observation = self.observation.lock().await;
                observation.state = "disconnect_unknown".into();
                observation.message="Heartbeats stopped here. Observatory has not confirmed disconnection; check again when it is reachable.".into();
                return Err(error.into());
            }
        }
        saved.revoke_pending = false;
        saved.access_refused = false;
        if saved.startup_enabled {
            if let Err(error) = platform::remove_startup(&self.store.home, &saved.machine_id) {
                self.observation.lock().await.startup_error = Some(error.to_string());
            } else {
                saved.startup_enabled = false;
            }
        }
        self.store.save("state.json", &*saved)?;
        let mut observation = self.observation.lock().await;
        observation.state = "disconnected".into();
        observation.message =
            "Disconnected. Your agents and their connections are unchanged.".into();
        Ok(json!({"disconnected":true}))
    }

    pub async fn heartbeat_loop(self: Arc<Self>) {
        let mut backoff = 1;
        loop {
            tokio::select! {_ = tokio::time::sleep(Duration::from_secs(backoff)) => {}, _ = self.wake.notified() => {}}
            let _operation = self.operation.lock().await;
            let saved = self.saved.lock().await.clone();
            if saved.disconnected || saved.access_refused || saved.revoke_pending {
                backoff = 15;
                continue;
            }
            let Some(receipt) = saved.receipt else {
                backoff = 15;
                continue;
            };
            let remote = match Remote::new(self.bootstrap.lock().await.clone()) {
                Ok(r) => r,
                Err(_) => continue,
            };
            match crate::installation::pending(&self.store) {
                Ok(Some(result)) => {
                    match remote
                        .installation_result(&receipt, &self.secret, &result)
                        .await
                    {
                        Ok(()) => {
                            if let Err(error) =
                                crate::installation::acknowledged(&self.store, &result)
                            {
                                self.observation.lock().await.message = error.to_string();
                                backoff = 15;
                                continue;
                            }
                        }
                        Err(error) => {
                            self.observation.lock().await.message = error.message;
                            backoff = 15;
                            continue;
                        }
                    }
                }
                Ok(None) => (),
                Err(error) => {
                    self.observation.lock().await.message = error.to_string();
                    backoff = 15;
                    continue;
                }
            }
            match remote
                .heartbeat(&receipt, &self.secret, &platform::hostname())
                .await
            {
                Ok(reply) => {
                    backoff = reply.heartbeat_seconds;
                    let mut o = self.observation.lock().await;
                    o.state = "connected".into();
                    o.last_seen = Some(now());
                    o.message = "This computer is connected.".into();
                    drop(o);
                    if let Some(request) = reply.installation {
                        if !receipt
                            .access
                            .contains(&crate::paired_protocol::INSTALL_ACCESS.to_owned())
                            || request.host_id != receipt.host_id
                            || request.machine_id != receipt.machine_id
                            || request.pairing_id != receipt.pairing_id
                        {
                            self.observation.lock().await.message="An installation request did not match this computer's saved access.".into();
                            continue;
                        }
                        match crate::installation::install(&self.store, &remote.bootstrap, &request)
                            .await
                        {
                            Ok(result) => {
                                self.observation.lock().await.message = result.message.clone();
                                if remote
                                    .installation_result(&receipt, &self.secret, &result)
                                    .await
                                    .is_ok()
                                {
                                    if let Err(error) =
                                        crate::installation::acknowledged(&self.store, &result)
                                    {
                                        self.observation.lock().await.message = error.to_string();
                                    }
                                }
                            }
                            Err(error) => self.observation.lock().await.message = error.to_string(),
                        }
                        backoff = 1;
                    }
                }
                Err(error) => {
                    let mut o = self.observation.lock().await;
                    o.message = error.message;
                    if matches!(error.status, Some(401 | 403 | 404)) {
                        o.state = "access_refused".into();
                        drop(o);
                        let mut saved = self.saved.lock().await;
                        saved.access_refused = true;
                        let _ = self.store.save("state.json", &*saved);
                    } else {
                        o.state = "offline".into();
                        backoff = (backoff * 2).min(120);
                    }
                }
            }
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairInput {
    code: String,
    #[serde(default)]
    install_agents: bool,
    #[serde(default)]
    startup: bool,
}
type HttpResult = std::result::Result<Json<Value>, (StatusCode, Json<Value>)>;
fn error(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":e.to_string()})),
    )
}
fn authenticate(app: &App, headers: &HeaderMap, write: bool) -> Result<()> {
    let host = app.origin.strip_prefix("http://").unwrap();
    ensure!(
        headers.get(header::HOST).and_then(|v| v.to_str().ok()) == Some(host),
        "Open setup on this computer"
    );
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    ensure!(
        !write || origin == Some(&app.origin),
        "Setup requires this local page"
    );
    ensure!(
        origin.is_none() || origin == Some(&app.origin),
        "Setup does not accept another website"
    );
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    let mut difference = token.len() ^ app.token.len();
    for (a, b) in token.bytes().zip(app.token.bytes()) {
        difference |= (a ^ b) as usize;
    }
    ensure!(
        difference == 0,
        "Reopen the machine helper to use this setup page"
    );
    Ok(())
}
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/setup.js", get(script))
        .route("/setup.css", get(style))
        .route("/nunito.woff2", get(font))
        .route("/api/status", get(status))
        .route("/api/pair", post(pair))
        .route("/api/installation-access", post(installation_access))
        .route("/api/disconnect", post(disconnect))
        .route("/api/bootstrap", post(bootstrap))
        .route("/api/startup", post(startup))
        .layer(DefaultBodyLimit::max(131072))
        .layer(axum::middleware::map_response(headers))
        .with_state(app)
}
async fn headers(mut response: Response) -> Response {
    let h = response.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY,"default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    h.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response
}
async fn index() -> Html<&'static str> {
    Html(include_str!("../ui/index.html"))
}
async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../ui/setup.js"),
    )
}
async fn style() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../ui/setup.css"),
    )
}
async fn font() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "font/woff2")],
        include_bytes!("../ui/nunito.woff2").as_slice(),
    )
}
async fn status(State(app): State<Arc<App>>, headers: HeaderMap) -> HttpResult {
    authenticate(&app, &headers, false).map_err(error)?;
    Ok(Json(app.status().await))
}
async fn pair(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(input): Json<PairInput>,
) -> HttpResult {
    authenticate(&app, &headers, true).map_err(error)?;
    app.pair(input).await.map(Json).map_err(error)
}
async fn installation_access(State(app): State<Arc<App>>, headers: HeaderMap) -> HttpResult {
    authenticate(&app, &headers, true).map_err(error)?;
    let _operation = app.operation.lock().await;
    let mut saved = app.saved.lock().await;
    if saved.disconnected || saved.revoke_pending || saved.access_refused {
        return Err(error("Reconnect this computer first"));
    }
    let receipt = saved
        .receipt
        .as_ref()
        .ok_or_else(|| error("Pair this computer first"))?;
    let remote = Remote::new(app.bootstrap.lock().await.clone()).map_err(error)?;
    let reply = remote
        .installation_access(receipt, &app.secret)
        .await
        .map_err(error)?;
    saved.receipt = Some(reply);
    app.store.save("state.json", &*saved).map_err(error)?;
    app.wake.notify_one();
    Ok(Json(json!({"accepted":true})))
}
async fn disconnect(State(app): State<Arc<App>>, headers: HeaderMap) -> HttpResult {
    authenticate(&app, &headers, true).map_err(error)?;
    app.unpair().await.map(Json).map_err(error)
}
async fn bootstrap(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(input): Json<Bootstrap>,
) -> HttpResult {
    authenticate(&app, &headers, true).map_err(error)?;
    input.validate().map_err(error)?;
    let _operation = app.operation.try_lock().map_err(error)?;
    let saved = app.saved.lock().await;
    let mut current = app.bootstrap.lock().await;
    if input.observatory_id != current.observatory_id
        || input.observatory_url != current.observatory_url
        || input.ca_pem != current.ca_pem
    {
        return Err(error("This helper belongs to a different Observatory"));
    }
    if saved.pending.is_some() {
        return Err(error(
            "Check the saved pairing request before using a new download",
        ));
    }
    if saved.receipt.is_some() && !saved.disconnected && !saved.access_refused {
        return Ok(Json(json!({"already_connected":true})));
    }
    *current = input;
    Ok(Json(json!({"ready":true})))
}
async fn startup(State(app): State<Arc<App>>, headers: HeaderMap) -> HttpResult {
    authenticate(&app, &headers, true).map_err(error)?;
    let _operation = app.operation.try_lock().map_err(error)?;
    let mut saved = app.saved.lock().await;
    if saved.disconnected || saved.access_refused || saved.receipt.is_none() {
        return Err(error("Pair this computer before enabling startup"));
    }
    platform::install_startup(&app.store.home, &saved.machine_id).map_err(error)?;
    saved.startup_enabled = true;
    app.store.save("state.json", &*saved).map_err(error)?;
    app.observation.lock().await.startup_error = None;
    Ok(Json(json!({"enabled":true})))
}
