//! Native first launch. The browser is local; existing installations retain their
//! own authentication, transport and state. No fleet or agent action is implied.
use crate::{
    auth,
    config::{self, AuthMode, Config, WebTransport},
    server,
    state::AppState,
};
use amplifier_native_platform::{self as platform, OBSERVATORY};
use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::any,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::TcpListener;
use tower::ServiceExt;

#[derive(Clone, Serialize, Deserialize)]
struct SetupSession {
    origin: String,
    token: String,
    expires_at: i64,
}
type ReadyInstallation = (uuid::Uuid, Arc<AppState>, Option<String>, File);

struct SetupState {
    home: PathBuf,
    session: SetupSession,
    ready: Mutex<Option<ReadyInstallation>>,
    creation: tokio::sync::Mutex<()>,
    shutdown: tokio::sync::watch::Sender<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    request_id: uuid::Uuid,
    name: String,
    password: String,
    start_at_signin: bool,
}

pub fn default_home() -> Result<PathBuf> {
    platform::default_home(&OBSERVATORY, "default")
}

fn service_lock(home: &Path) -> Result<File> {
    let path = home.join("service.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    platform::protect_file(&path)?;
    file.try_lock_exclusive()
        .context("Observatory is already running")?;
    Ok(file)
}

pub async fn local_api(
    home: &Path,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    ensure!(
        !path.starts_with('/') && !path.contains(".."),
        "Use a relative API path"
    );
    let c = Config::load(home)?;
    let token = std::fs::read_to_string(home.join("secrets/control-token"))?;
    if c.web_transport == WebTransport::LocalHttp {
        return tokio::time::timeout(
            Duration::from_secs(15),
            proved_local_api(&c, token.trim(), method, path, body),
        )
        .await
        .context("Local Observatory did not respond in time")?;
    }
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15));
    let ca = std::fs::read(c.tls_ca.as_ref().unwrap_or(&c.tls_cert))?;
    builder = builder.add_root_certificate(reqwest::Certificate::from_pem(&ca)?);
    let base = format!("https://127.0.0.1:{}", c.port);
    let mut request = builder
        .build()?
        .request(method.parse()?, format!("{base}/api/v1/{path}"))
        .bearer_auth(token.trim());
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    let status = response.status();
    let result: serde_json::Value = response.json().await?;
    ensure!(
        status.is_success(),
        "{}",
        result["error"].as_str().unwrap_or("Request failed")
    );
    Ok(result)
}

// Keep proof and authenticated request on one connection. A client pool could
// reconnect to a different process after proof and disclose the credential.
async fn proved_local_api(
    config: &Config,
    token: &str,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    use axum::body::Bytes;
    use http_body_util::{BodyExt, Full, Limited};
    use hyper_util::rt::TokioIo;

    struct Connection(tokio::task::JoinHandle<()>);
    impl Drop for Connection {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", config.port)).await?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream)).await?;
    let _connection = Connection(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let host = config
        .public_url
        .strip_prefix("http://")
        .context("Invalid local origin")?;
    let nonce = auth::random_token();
    let proof_request = hyper::Request::builder()
        .method("GET")
        .uri(format!("/api/v1/local-proof?nonce={nonce}"))
        .header(header::HOST, host)
        .body(Full::new(Bytes::new()))?;
    let response = sender.send_request(proof_request).await?;
    ensure!(
        response.status().is_success(),
        "Local installation proof was refused"
    );
    let bytes = Limited::new(response.into_body(), 4096)
        .collect()
        .await
        .map_err(|_| anyhow::anyhow!("Invalid local installation proof response"))?
        .to_bytes();
    let proof: serde_json::Value = serde_json::from_slice(&bytes)?;
    ensure!(
        auth::verify_local_proof(config, &nonce, token, &proof),
        "A different installation answered at the saved address"
    );
    let mut request = hyper::Request::builder()
        .method(method)
        .uri(format!("/api/v1/{path}"))
        .header(header::HOST, host)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    let bytes = if let Some(body) = body {
        request = request.header(header::CONTENT_TYPE, "application/json");
        serde_json::to_vec(&body)?
    } else {
        Vec::new()
    };
    let response = sender
        .send_request(request.body(Full::new(Bytes::from(bytes)))?)
        .await?;
    let status = response.status();
    let bytes = Limited::new(response.into_body(), 16 * 1024 * 1024)
        .collect()
        .await
        .map_err(|_| anyhow::anyhow!("Invalid or excessive local API response"))?
        .to_bytes();
    let result: serde_json::Value = serde_json::from_slice(&bytes)?;
    ensure!(
        status.is_success(),
        "{}",
        result["error"].as_str().unwrap_or("Request failed")
    );
    Ok(result)
}

fn open_installed(url: &str) -> Result<()> {
    if url.starts_with("https:") {
        platform::open_https_browser(url)
    } else {
        platform::open_browser(url)
    }
}

pub async fn serve(home: PathBuf, open_browser: bool) -> Result<()> {
    serve_guarded(home, open_browser, None).await
}
pub async fn serve_guarded(
    home: PathBuf,
    open_browser: bool,
    update: Option<(String, String)>,
) -> Result<()> {
    let _lock = service_lock(&home)?;
    if let Some((id, nonce)) = update.as_ref() {
        crate::desktop_update::validate_start(&home, id, nonce)?;
    } else {
        crate::desktop_update::direct_start_allowed(&home)?;
    }
    let state = AppState::open(home.clone())?;
    let mut shutdown = state.shutdown.subscribe();
    let url = if state.config.public_url.is_empty() {
        format!("https://localhost:{}", state.config.port)
    } else {
        state.config.public_url.clone()
    };
    if state.config.web_transport == WebTransport::LocalHttp {
        let listener = TcpListener::bind(("127.0.0.1", state.config.port)).await?;
        record_runtime(&state)?;
        crate::machine_network::restore(&state).await;
        start_eviction(&state);
        if open_browser {
            open_installed(&url)?;
        }
        axum::serve(listener, server::router(state))
            .with_graceful_shutdown(async move {
                let _ = shutdown.changed().await;
            })
            .await?;
    } else {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            &state.config.tls_cert,
            &state.config.tls_key,
        )
        .await?;
        let address: std::net::SocketAddr =
            format!("{}:{}", state.config.bind, state.config.port).parse()?;
        let listener = std::net::TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        record_runtime(&state)?;
        start_eviction(&state);
        if open_browser {
            open_installed(&url)?;
        }
        let handle = axum_server::Handle::new();
        let signal = handle.clone();
        tokio::spawn(async move {
            let _ = shutdown.changed().await;
            signal.graceful_shutdown(Some(Duration::from_secs(30)));
        });
        axum_server::from_tcp_rustls(listener, tls)?
            .handle(handle)
            .serve(server::router(state).into_make_service())
            .await?;
    }
    Ok(())
}

pub fn binary_digest() -> Result<String> {
    use sha2::{Digest, Sha256};
    static DIGEST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(value) = DIGEST.get() {
        return Ok(value.clone());
    }
    let value = format!(
        "{:x}",
        Sha256::digest(std::fs::read(std::env::current_exe()?)?)
    );
    let _ = DIGEST.set(value.clone());
    Ok(value)
}
fn record_runtime(state: &AppState) -> Result<()> {
    config::private_write(
        &state.home.join("runtime.json"),
        &serde_json::to_vec(
            &json!({"pid":std::process::id(),"port":state.config.port,"url":state.config.public_url,"version":env!("CARGO_PKG_VERSION"),"binary_sha256":binary_digest()?}),
        )?,
    )
}
fn start_eviction(state: &Arc<AppState>) {
    let weak = Arc::downgrade(state);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let Some(state) = weak.upgrade() else { break };
            crate::acp::evict_idle(&state);
        }
    });
}

/// Opening the native application again opens the existing installation. A
/// failed identity check does not replace it or start a competing service.
pub async fn launch(home: PathBuf, no_browser: bool) -> Result<()> {
    if home.join("config.json").exists() {
        let c = Config::load(&home)?;
        if crate::desktop_update::pending(&home) {
            return crate::desktop_update::offer(home, no_browser).await;
        }
        if let Ok(installed) = std::fs::read(home.join("app-install.json")) {
            let installed: serde_json::Value = serde_json::from_slice(&installed)?;
            if binary_digest()? != installed["binary_sha256"] {
                crate::desktop_update::package(&std::env::current_exe()?)?;
                return crate::desktop_update::offer(home, no_browser).await;
            }
        }
        if let Ok(status) = local_api(&home, "GET", "status", None).await {
            ensure!(
                status["id"] == c.id,
                "A different installation answered at the saved address"
            );
            let url = if c.public_url.is_empty() {
                format!("https://localhost:{}", c.port)
            } else {
                c.public_url
            };
            if no_browser {
                println!("{}", json!({"state":"running","url":url,"id":c.id}));
            } else {
                open_installed(&url)?;
            }
            return Ok(());
        }
        return serve(home, !no_browser).await;
    }
    ensure!(
        !home.exists(),
        "This folder already contains files. They have been preserved; choose a new Observatory folder."
    );
    let parent = home
        .parent()
        .context("Choose a complete installation path")?;
    std::fs::create_dir_all(parent)?;
    let leaf = home
        .file_name()
        .context("Choose a named installation folder")?
        .to_string_lossy();
    let identity = auth::digest(&home.to_string_lossy());
    // Protect only our own launch directory, never the caller's parent (which
    // may be a shared directory such as /tmp).
    let control = parent.join(format!(".observatory-launch-{}", &identity[..16]));
    config::private_dir(&control)?;
    let lock_path = control.join("setup.lock");
    let session_path = control.join("setup-session.json");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    platform::protect_file(&lock_path)?;
    if lock.try_lock_exclusive().is_err() {
        let session: SetupSession =
            serde_json::from_slice(&std::fs::read(&session_path).context(
                "Observatory setup is already opening; try opening the app again in a moment",
            )?)?;
        ensure!(
            session.expires_at > crate::state::now_ms(),
            "The existing setup has expired; close its app before opening setup again"
        );
        let url = reqwest::Url::parse(&session.origin)?;
        ensure!(
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port().is_some()
                && url.path() == "/",
            "Invalid local setup address"
        );
        let url = format!("{}/#{}", session.origin, session.token);
        if no_browser {
            println!("{}", json!({"state":"setup","url":url}));
        } else {
            platform::open_browser(&url)?;
        }
        return Ok(());
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let session = SetupSession {
        origin: format!("http://127.0.0.1:{}", listener.local_addr()?.port()),
        token: auth::random_token(),
        expires_at: crate::state::now_ms() + 30 * 60 * 1000,
    };
    config::private_write(&session_path, &serde_json::to_vec(&session)?)?;
    let state = Arc::new(SetupState {
        home: home.clone(),
        session: session.clone(),
        ready: Mutex::new(None),
        creation: tokio::sync::Mutex::new(()),
        shutdown: tokio::sync::watch::channel(false).0,
    });
    let url = format!("{}/#{}", session.origin, session.token);
    if no_browser {
        println!(
            "{}",
            json!({"state":"setup","url":url,"home":home,"name":leaf})
        );
    } else {
        platform::open_browser(&url)?;
    }
    let mut shutdown = state.shutdown.subscribe();
    let result = axum::serve(
        listener,
        Router::new().fallback(any(dispatch)).with_state(state),
    )
    .with_graceful_shutdown(async move {
        let _ = shutdown.changed().await;
    })
    .await;
    // Only our exact local launch capability is removed; no installed state is rolled back.
    if std::fs::read(&session_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<SetupSession>(&b).ok())
        .is_some_and(|s| s.token == session.token)
    {
        let _ = std::fs::remove_file(session_path);
    }
    drop(lock);
    result?;
    Ok(())
}

async fn dispatch(State(state): State<Arc<SetupState>>, request: Request) -> Response {
    let host = state.session.origin.trim_start_matches("http://");
    let headers = request.headers();
    if headers.get(header::HOST).and_then(|h| h.to_str().ok()) != Some(host)
        || headers
            .get(header::ORIGIN)
            .is_some_and(|o| o.to_str().ok() != Some(state.session.origin.as_str()))
        || headers
            .get("sec-fetch-site")
            .is_some_and(|s| s == "cross-site")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let route = request.uri().path().to_owned();
    if route == "/desktop/create" || route == "/desktop/status" {
        let expected = format!("Bearer {}", state.session.token);
        if state.session.expires_at <= crate::state::now_ms()
            || headers
                .get(header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                != Some(expected.as_str())
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if route == "/desktop/status" && request.method() == "GET" {
            let ready = state.ready.lock().unwrap();
            return axum::Json(json!({"ready":ready.is_some(),"startup_warning":ready.as_ref().and_then(|r|r.2.clone())})).into_response();
        }
        if route == "/desktop/create" && request.method() == "POST" {
            let bytes = match to_bytes(request.into_body(), 8192).await {
                Ok(b) => b,
                Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
            };
            let create: Create = match serde_json::from_slice(&bytes) {
                Ok(c) => c,
                Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            };
            return create_installation(state, create).await;
        }
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let ready = state.ready.lock().unwrap().as_ref().map(|r| r.1.clone());
    if let Some(ready) = ready {
        return server::router(ready)
            .oneshot(request)
            .await
            .unwrap_or_else(|never| match never {});
    }
    if request.method() != "GET" {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let response = match route.as_str() {
        "/" => Html(include_str!("../ui/desktop-setup.html")).into_response(),
        "/desktop/setup.js" => (
            [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
            include_str!("../ui/desktop-setup.js"),
        )
            .into_response(),
        "/desktop/setup.css" => (
            [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
            include_str!("../ui/desktop-setup.css"),
        )
            .into_response(),
        "/desktop/nunito.woff2" => (
            [(header::CONTENT_TYPE, "font/woff2")],
            include_bytes!("../ui/world-assets/nunito.woff2").as_slice(),
        )
            .into_response(),
        "/desktop/frame.webp" => (
            [(header::CONTENT_TYPE, "image/webp")],
            include_bytes!("../ui/world-assets/frame.webp").as_slice(),
        )
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    };
    let mut response = response;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
            .parse()
            .unwrap(),
    );
    response
}

async fn create_installation(state: Arc<SetupState>, create: Create) -> Response {
    let _creation = state.creation.lock().await;
    if let Some((id, ready, _, _)) = state.ready.lock().unwrap().as_ref() {
        if *id != create.request_id {
            return StatusCode::CONFLICT.into_response();
        }
        return auth::signed_in(ready, &ready.config.owner, "local-setup")
            .unwrap_or_else(IntoResponse::into_response);
    }
    let owner = create.name.trim().to_owned();
    if owner.is_empty() || owner.len() >= 100 || owner.chars().any(char::is_control) {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error":"Enter a name for your Observatory account."})),
        )
            .into_response();
    }
    if let Err(error) = auth::validate_local_password(&create.password) {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    let home = state.home.clone();
    let origin = state.session.origin.clone();
    let id = create.request_id;
    let result=tokio::task::spawn_blocking(move || -> Result<(Arc<AppState>,Option<String>,File)> {
        ensure!(!home.exists(),"An installation already exists. Its files have been preserved.");
        let parent=home.parent().context("Missing installation parent")?;
        let staged=parent.join(format!(".observatory-install-{id}"));
        ensure!(!staged.exists(),"An earlier setup is incomplete. Its files have been preserved for recovery.");
        config::private_dir(&staged)?;
        let url=reqwest::Url::parse(&origin)?;
        let c=Config {id:uuid::Uuid::new_v4().to_string(),owner,bind:"127.0.0.1".into(),port:url.port().context("Missing local port")?,public_url:origin,auth_mode:AuthMode::LocalPassword,web_transport:WebTransport::LocalHttp,pam_service:String::new(),tls_cert:home.join("tls/cert.pem"),tls_key:home.join("tls/key.pem"),tls_ca:None,embed_origins:vec![]};
        auth::initialize_local_password(&staged,&create.password)?;
        c.save(&staged)?;
        // Initialize then close SQLite before moving its private directory (also on Windows).
        let staged_state=AppState::open(staged.clone())?;
        let bundle=crate::local_install::bundled_path()?;
        crate::local_install::prepare(&staged_state,&home,bundle.as_deref(),&crate::local_install::default_agent_root()?)?;
        crate::desktop_update::retain(&staged_state)?;
        drop(staged_state);
        ensure!(!home.exists(),"The destination changed during setup; the staged files were preserved.");
        platform::publish_directory_if_absent(&staged,&home)?;
        let service=service_lock(&home)?;
        let ready=AppState::open(home.clone())?;
        let warning=if create.start_at_signin {platform::install_startup(&OBSERVATORY,&home,&uuid::Uuid::parse_str(&c.id)?).err().map(|_|"Observatory is ready, but sign-in startup could not be enabled. Open the app to run it.".to_owned())}else{None};
        config::private_write(&home.join("desktop-setup.json"),&serde_json::to_vec(&json!({"schema":1,"request_id":id,"installation_id":c.id,"ready":true,"startup_requested":create.start_at_signin,"startup_enabled":create.start_at_signin&&warning.is_none(),"startup_warning":warning}))?)?;
        Ok((ready,warning,service))
    }).await;
    match result {
        Ok(Ok((ready,warning,service)))=>{
            if let Err(error)=record_runtime(&ready) {return (StatusCode::INTERNAL_SERVER_ERROR,axum::Json(json!({"error":error.to_string()}))).into_response()}
            start_eviction(&ready);
            let mut shutdown=ready.shutdown.subscribe();let setup_shutdown=state.shutdown.clone();
            tokio::spawn(async move {if shutdown.changed().await.is_ok(){setup_shutdown.send_replace(true);}});
            let response=auth::signed_in(&ready,&ready.config.owner,"local-setup").unwrap_or_else(IntoResponse::into_response);
            *state.ready.lock().unwrap()=Some((id,ready,warning,service));
            response
        }
        Ok(Err(error))=>(StatusCode::CONFLICT,axum::Json(json!({"error":error.to_string()}))).into_response(),
        Err(_)=>(StatusCode::INTERNAL_SERVER_ERROR,axum::Json(json!({"error":"Setup did not finish. Your files were preserved; reopen Observatory to inspect its state."}))).into_response(),
    }
}
