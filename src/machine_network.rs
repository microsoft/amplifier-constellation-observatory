//! Optional helper-only TLS on one explicitly selected local address.
//! The local browser, account and existing HTTPS installations remain independent.
use crate::{
    auth,
    config::{self, WebTransport},
    error::{ApiError, ApiResult},
    state::AppState,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
    time::Duration,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    schema: u32,
    revision: u64,
    request_id: uuid::Uuid,
    enabled: bool,
    address: IpAddr,
    port: u16,
}
impl Settings {
    fn endpoint(&self) -> String {
        format!("https://{}", SocketAddr::new(self.address, self.port))
    }
}
#[derive(Default)]
pub struct Runtime {
    handle: Option<axum_server::Handle<SocketAddr>>,
    task: Option<tokio::task::JoinHandle<()>>,
    endpoint: Option<String>,
    error: Option<String>,
}
impl Runtime {
    fn running(&self) -> bool {
        self.task.as_ref().is_some_and(|t| !t.is_finished())
    }
    fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.graceful_shutdown(Some(Duration::from_secs(5)));
        }
        self.endpoint = None;
    }
}
fn settings(home: &Path) -> Result<Option<Settings>> {
    let path = home.join("machine-network.json");
    let metadata = match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(value) => value,
    };
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 16384,
        "Network setup needs inspection; its files were preserved"
    );
    let value: Settings = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(
        value.schema == 1 && value.revision > 0 && value.port != 0 && usable(value.address),
        "Invalid saved network setup"
    );
    Ok(Some(value))
}
fn usable(ip: IpAddr) -> bool {
    !ip.is_unspecified()
        && !ip.is_loopback()
        && !ip.is_multicast()
        && !matches!(ip, IpAddr::V6(v) if v.is_unicast_link_local())
}
fn addresses() -> Result<Vec<Value>> {
    let mut values = Vec::new();
    for interface in if_addrs::get_if_addrs()? {
        let address = interface.ip();
        if usable(address)
            && !values
                .iter()
                .any(|v: &Value| v["address"] == address.to_string())
        {
            values.push(json!({"address":address.to_string(),"label":format!("{} · {}",interface.name,address)}));
        }
    }
    Ok(values)
}
fn is_current_address(address: IpAddr) -> Result<bool> {
    Ok(addresses()?
        .iter()
        .any(|value| value["address"] == address.to_string()))
}
fn ca_params(id: &str) -> Result<rcgen::CertificateParams> {
    use rcgen::{BasicConstraints, DistinguishedName, DnType, IsCa, KeyUsagePurpose};
    let mut params = rcgen::CertificateParams::new(vec![])?;
    let mut name = DistinguishedName::new();
    name.push(
        DnType::CommonName,
        format!("Observatory computer pairing {id}"),
    );
    params.distinguished_name = name;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    Ok(params)
}
fn certificates(home: &Path, id: &str, address: IpAddr) -> Result<(String, String)> {
    use rcgen::{ExtendedKeyUsagePurpose, Issuer, KeyPair};
    let root = home.join("machine-network-tls");
    config::private_dir(&root)?;
    let key_path = root.join("ca.key");
    let cert_path = root.join("ca.pem");
    let params = ca_params(id)?;
    let key = if key_path.exists() || cert_path.exists() {
        ensure!(
            key_path.is_file()
                && cert_path.is_file()
                && !key_path.is_symlink()
                && !cert_path.is_symlink(),
            "Existing network certificate files need inspection"
        );
        KeyPair::from_pem(&std::fs::read_to_string(&key_path)?)?
    } else {
        let key = KeyPair::generate()?;
        let cert = params.self_signed(&key)?;
        config::private_write(&key_path, key.serialize_pem().as_bytes())?;
        config::private_write(&cert_path, cert.pem().as_bytes())?;
        key
    };
    let issuer = Issuer::new(params, key);
    let mut leaf = rcgen::CertificateParams::new(vec![address.to_string()])?;
    leaf.use_authority_key_identifier_extension = true;
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key = KeyPair::generate()?;
    let cert = leaf.signed_by(&key, &issuer)?;
    Ok((cert.pem(), key.serialize_pem()))
}
async fn prepared(
    s: &AppState,
    address: IpAddr,
    port: u16,
) -> Result<(std::net::TcpListener, axum_server::tls_rustls::RustlsConfig)> {
    ensure!(
        is_current_address(address)?,
        "That network address is unavailable. Choose an address currently on this computer."
    );
    let listener = std::net::TcpListener::bind(SocketAddr::new(address, port)).context(
        "The saved network address or port is unavailable; the local Observatory still works",
    )?;
    listener.set_nonblocking(true)?;
    let (cert, key) = certificates(&s.home, &s.config.id, address)?;
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(cert.into_bytes(), key.into_bytes())
        .await?;
    Ok((listener, tls))
}
fn launch(
    s: &Arc<AppState>,
    runtime: &mut Runtime,
    listener: std::net::TcpListener,
    tls: axum_server::tls_rustls::RustlsConfig,
    endpoint: String,
) -> Result<()> {
    let handle = axum_server::Handle::new();
    let server = axum_server::from_tcp_rustls(listener, tls)?;
    let router = crate::machines::helper_routes()
        .layer(DefaultBodyLimit::max(32768))
        .layer(axum::middleware::from_fn_with_state(
            s.clone(),
            |State(s): State<Arc<AppState>>,
             request: axum::extract::Request,
             next: axum::middleware::Next| async move {
                use axum::response::IntoResponse;
                if request.headers().contains_key(axum::http::header::ORIGIN) {
                    return StatusCode::FORBIDDEN.into_response();
                }
                if let Some(response) =
                    crate::server::update_hold_response(&s, request.method(), request.uri().path())
                {
                    return response;
                }
                next.run(request).await
            },
        ))
        .with_state(s.clone());
    let mut shutdown = s.shutdown.subscribe();
    let signal = handle.clone();
    let finished = handle.clone();
    let task = tokio::spawn(async move {
        let running = server.handle(signal).serve(router.into_make_service());
        tokio::pin!(running);
        tokio::select! {
            _ = &mut running => (),
            _ = shutdown.changed() => {
                finished.graceful_shutdown(Some(Duration::from_secs(5)));
                let _ = running.await;
            }
        }
    });
    runtime.stop();
    runtime.handle = Some(handle);
    runtime.task = Some(task);
    runtime.endpoint = Some(endpoint);
    runtime.error = None;
    Ok(())
}
/// Listener errors must not prevent the person's local Observatory from opening.
pub async fn restore(s: &Arc<AppState>) {
    if s.config.web_transport != WebTransport::LocalHttp {
        return;
    }
    let mut runtime = s.machine_network.lock().await;
    let result: Result<()> = async {
        let Some(saved) = settings(&s.home)? else {
            return Ok(());
        };
        if saved.enabled {
            let (listener, tls) = prepared(s, saved.address, saved.port).await?;
            launch(s, &mut runtime, listener, tls, saved.endpoint())?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        runtime.error = Some(error.to_string());
    }
}
pub async fn endpoint(s: &AppState) -> ApiResult<(String, Option<String>)> {
    if s.config.web_transport == WebTransport::Https {
        if !config::valid_origin(&s.config.public_url) {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Set this Observatory's reachable HTTPS address before pairing a computer".into(),
            ));
        }
        return Ok((
            s.config.public_url.clone(),
            s.config
                .tls_ca
                .as_ref()
                .map(std::fs::read_to_string)
                .transpose()?,
        ));
    }
    let runtime = s.machine_network.lock().await;
    let saved = settings(&s.home)
        .map_err(|error| ApiError::bad(error.to_string()))?
        .ok_or_else(|| {
            ApiError(
                StatusCode::CONFLICT,
                "Choose a network in Add a computer first".into(),
            )
        })?;
    if !saved.enabled
        || !runtime.running()
        || !is_current_address(saved.address).unwrap_or(false)
        || runtime.endpoint.as_deref() != Some(saved.endpoint().as_str())
    {
        return Err(ApiError(StatusCode::CONFLICT,
            "Computer pairing is unavailable on the saved network. Your local Observatory still works.".into(),
        ));
    }
    Ok((
        saved.endpoint(),
        Some(std::fs::read_to_string(
            s.home.join("machine-network-tls/ca.pem"),
        )?),
    ))
}
async fn projection(s: &AppState) -> Value {
    if s.config.web_transport == WebTransport::Https {
        return json!({"mode":"configured_https","enabled":true,"available":config::valid_origin(&s.config.public_url),"endpoint":s.config.public_url,"revision":0,"addresses":[]});
    }
    let runtime = s.machine_network.lock().await;
    let saved = settings(&s.home);
    let error = saved
        .as_ref()
        .err()
        .map(ToString::to_string)
        .or_else(|| runtime.error.clone());
    let saved = saved.ok().flatten();
    let enabled = saved.as_ref().is_some_and(|v| v.enabled);
    let address_available = saved
        .as_ref()
        .is_some_and(|v| is_current_address(v.address).unwrap_or(false));
    let error = error.or_else(|| {
        if enabled && !address_available {
            Some(
                "The saved network address is unavailable. Your local Observatory still works."
                    .to_owned(),
            )
        } else if enabled && !runtime.running() {
            Some("Computer pairing is not listening. Check the network and try again.".to_owned())
        } else {
            None
        }
    });
    json!({"mode":"local","enabled":enabled,"available":enabled&&runtime.running()&&address_available,"endpoint":saved.as_ref().map(Settings::endpoint),"address":saved.as_ref().map(|v|v.address),"revision":saved.as_ref().map_or(0,|v|v.revision),"request_id":saved.as_ref().map(|v|v.request_id),"error":error,"addresses":addresses().unwrap_or_default()})
}
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/machine-network", get(status).post(configure))
}
async fn status(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "provision", false)?;
    Ok(Json(projection(&s).await))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    request_id: uuid::Uuid,
    expected_revision: u64,
    enabled: bool,
    address: Option<IpAddr>,
}
async fn configure(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(input): Json<Change>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    if s.config.web_transport != WebTransport::LocalHttp {
        return Err(ApiError::bad(
            "This Observatory already uses its configured HTTPS address",
        ));
    }
    let mut runtime = s.machine_network.lock().await;
    let old = settings(&s.home).map_err(|error| ApiError::bad(error.to_string()))?;
    if let Some(old) = &old
        && old.request_id == input.request_id
    {
        if old.enabled != input.enabled || (input.enabled && input.address != Some(old.address)) {
            return Err(ApiError::bad(
                "This network request already has a different saved result",
            ));
        }
        drop(runtime);
        return Ok(Json(projection(&s).await));
    }
    if old.as_ref().map_or(0, |v| v.revision) != input.expected_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Network setup changed. Check its current status before continuing.".into(),
        ));
    }
    let result: Result<()> = async {
        let address = input.address.or_else(|| old.as_ref().map(|v|v.address)).context("Choose this computer's network address")?;
        ensure!(usable(address), "Choose a current network address");
        let port = old.as_ref().map_or(0, |v|v.port);
        let unchanged_live = input.enabled && runtime.running() && old.as_ref().is_some_and(|v|v.address == address && v.enabled);
        let prepared = if input.enabled && !unchanged_live { Some(prepared(&s, address, port).await?) } else { None };
        let port = prepared.as_ref().map(|(listener,_)|listener.local_addr().map(|v|v.port())).transpose()?.unwrap_or(port);
        ensure!(port != 0, "There is no network setup to disable");
        let next = Settings { schema:1, revision: input.expected_revision + 1, request_id:input.request_id, enabled:input.enabled, address, port };
        config::private_write(&s.home.join("machine-network.json"), &serde_json::to_vec(&next)?)?;
        if let Some((listener,tls)) = prepared { launch(&s, &mut runtime, listener, tls, next.endpoint())?; }
        if !input.enabled { runtime.stop(); runtime.error=None; }
        s.audit("machine_network_changed", None, json!({"enabled":next.enabled,"address":next.address,"port":next.port,"request_id":next.request_id}))?;
        Ok(())
    }.await;
    if let Err(error) = result {
        runtime.error = Some(error.to_string());
        return Err(ApiError::bad(error.to_string()));
    }
    drop(runtime);
    Ok(Json(projection(&s).await))
}
