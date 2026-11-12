//! Private native-update admission and package identity; no agent/model calls.
use agent_observatory::{
    config::{AuthMode, Config, WebTransport},
    desktop, desktop_update,
    state::AppState,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
fn hash(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}
fn setup() -> (tempfile::TempDir, Arc<AppState>) {
    let home = tempfile::tempdir().unwrap();
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        owner: "fixture".into(),
        bind: "127.0.0.1".into(),
        port: 1,
        public_url: "http://127.0.0.1:1".into(),
        auth_mode: AuthMode::LocalPassword,
        web_transport: WebTransport::LocalHttp,
        pam_service: String::new(),
        tls_cert: PathBuf::new(),
        tls_key: PathBuf::new(),
        tls_ca: None,
        embed_origins: vec![],
    }
    .save(home.path())
    .unwrap();
    let s = AppState::open(home.path().into()).unwrap();
    (home, s)
}
fn journal(home: &Path, s: &AppState, phase: &str) -> Value {
    let j = json!({"request_id":uuid::Uuid::new_v4().to_string(),"installation_id":s.config.id,"phase":phase,"launch_nonce":"original-nonce","candidate":{"version":env!("CARGO_PKG_VERSION"),"binary_sha256":desktop::binary_digest().unwrap()},"previous":{"version":env!("CARGO_PKG_VERSION"),"binary_sha256":desktop::binary_digest().unwrap()}});
    std::fs::write(
        home.join("desktop-update.json"),
        serde_json::to_vec(&j).unwrap(),
    )
    .unwrap();
    j
}
fn stop_body(j: &Value) -> Value {
    json!({"request_id":j["request_id"],"version":env!("CARGO_PKG_VERSION")})
}
#[test]
fn active_talk_and_install_are_not_cancelled_or_modified() {
    let (h, s) = setup();
    let j = journal(h.path(), &s, "prepared");
    let before = std::fs::read(h.path().join("desktop-update.json")).unwrap();
    s.active_sessions
        .lock()
        .unwrap()
        .insert("existing-turn".into());
    assert!(desktop_update::stop(&s, &stop_body(&j)).is_err());
    assert!(!s.operation_gate.is_closed());
    assert!(!*s.shutdown.borrow());
    s.active_sessions.lock().unwrap().clear();
    s.save(
        "jobs",
        json!({"id":"existing-job","status":"queued"}),
        Some(0),
    )
    .unwrap();
    assert!(desktop_update::stop(&s, &stop_body(&j)).is_err());
    assert_eq!(s.read("jobs", "existing-job").unwrap()["status"], "queued");
    assert_eq!(
        std::fs::read(h.path().join("desktop-update.json")).unwrap(),
        before
    );
}
#[test]
fn existing_operation_permit_defers_shutdown_and_wrong_request_never_closes() {
    let (h, s) = setup();
    let j = journal(h.path(), &s, "prepared");
    let permit = s.operation_gate.clone().try_acquire_owned().unwrap();
    assert!(desktop_update::stop(&s, &stop_body(&j)).is_err());
    assert!(!*s.shutdown.borrow());
    drop(permit);
    assert!(
        desktop_update::stop(
            &s,
            &json!({"request_id":uuid::Uuid::new_v4(),"version":env!("CARGO_PKG_VERSION")})
        )
        .is_err()
    );
    assert!(!s.operation_gate.is_closed());
    assert_eq!(
        desktop_update::stop(&s, &stop_body(&j)).unwrap()["accepted"],
        true
    );
    assert!(s.operation_gate.is_closed());
    assert!(*s.shutdown.borrow());
}
#[test]
fn candidate_admission_waits_for_matching_terminal_receipt() {
    let (h, s) = setup();
    let mut j = journal(h.path(), &s, "starting");
    drop(s);
    let s = AppState::open(h.path().into()).unwrap();
    assert!(desktop_update::busy(&s));
    assert!(s.operation_gate.clone().try_acquire_owned().is_err());
    assert!(desktop_update::finish(&s, &stop_body(&j)).is_err());
    j["phase"] = json!("applied");
    std::fs::write(
        h.path().join("desktop-update.json"),
        serde_json::to_vec(&j).unwrap(),
    )
    .unwrap();
    assert!(desktop_update::finish(&s, &json!({"request_id":"different"})).is_err());
    assert!(desktop_update::finish(&s, &stop_body(&j)).is_ok());
    assert!(desktop_update::finish(&s, &stop_body(&j)).is_ok());
    assert!(!desktop_update::busy(&s));
}
#[test]
fn rollback_invalidates_delayed_candidate_before_it_can_open_state() {
    let (h, s) = setup();
    let mut j = journal(h.path(), &s, "starting");
    let id = j["request_id"].as_str().unwrap();
    assert!(desktop_update::validate_start(h.path(), id, "original-nonce").is_ok());
    assert!(desktop_update::validate_start(h.path(), id, "old-nonce").is_err());
    assert!(desktop_update::direct_start_allowed(h.path()).is_err());
    assert!(desktop_update::pending(h.path()));
    j["phase"] = json!("recovering");
    std::fs::write(
        h.path().join("desktop-update.json"),
        serde_json::to_vec(&j).unwrap(),
    )
    .unwrap();
    assert!(
        desktop_update::validate_start(
            h.path(),
            j["request_id"].as_str().unwrap(),
            "original-nonce"
        )
        .is_err()
    );
}
#[test]
fn unreadable_or_corrupt_update_is_preserved_and_never_treated_as_absent() {
    let (home, _) = setup();
    let path = home.path().join("desktop-update.json");
    std::fs::write(&path, b"{unfinished").unwrap();
    assert!(desktop_update::pending(home.path()));
    assert!(desktop_update::direct_start_allowed(home.path()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{unfinished");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(desktop_update::pending(home.path()));
    assert!(desktop_update::direct_start_allowed(home.path()).is_err());
    assert!(path.is_dir());
}
#[test]
fn first_install_keeps_private_executable_without_signin_startup() {
    let (h, s) = setup();
    desktop_update::retain(&s).unwrap();
    let installed = amplifier_native_platform::installed_binary(
        &amplifier_native_platform::OBSERVATORY,
        h.path(),
    );
    assert_eq!(
        hash(&std::fs::read(installed).unwrap()),
        desktop::binary_digest().unwrap()
    );
    let m: Value =
        serde_json::from_slice(&std::fs::read(h.path().join("app-install.json")).unwrap()).unwrap();
    assert_eq!(m["id"], s.config.id);
    assert!(!h.path().join("startup.xml").exists());
}
#[test]
fn corrupt_or_wrong_platform_download_is_rejected_before_stage() {
    let temp = tempfile::tempdir().unwrap();
    let name = if cfg!(windows) {
        "agent-observatory.exe"
    } else {
        "agent-observatory"
    };
    let exe = temp.path().join(name);
    std::fs::write(&exe, b"candidate").unwrap();
    let target = format!(
        "{}-{}",
        std::env::consts::ARCH,
        if cfg!(windows) {
            "pc-windows-msvc"
        } else if cfg!(target_os = "macos") {
            "apple-darwin"
        } else {
            "unknown-linux-gnu"
        }
    );
    let mut m = json!({"schema":1,"component":"agent-observatory","version":env!("CARGO_PKG_VERSION"),"target":target,"binary":name,"binary_sha256":hash(b"candidate")});
    std::fs::write(
        temp.path().join("observatory-package.json"),
        serde_json::to_vec(&m).unwrap(),
    )
    .unwrap();
    assert!(desktop_update::package(&exe).is_ok());
    std::fs::write(&exe, b"changed").unwrap();
    assert!(desktop_update::package(&exe).is_err());
    std::fs::write(&exe, b"candidate").unwrap();
    m["target"] = json!("wrong-target");
    std::fs::write(
        temp.path().join("observatory-package.json"),
        serde_json::to_vec(&m).unwrap(),
    )
    .unwrap();
    assert!(desktop_update::package(&exe).is_err());
}
