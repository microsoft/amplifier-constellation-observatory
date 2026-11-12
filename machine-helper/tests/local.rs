use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use constellation_machine_helper::{
    app::{self, App},
    client::Bootstrap,
    platform,
    storage::Store,
};
use serde_json::json;
use std::{path::Path, sync::Arc};
use tower::ServiceExt;
use uuid::Uuid;
fn bootstrap() -> Bootstrap {
    serde_json::from_value(json!({"schema":1,"observatory_url":"https://observatory.example","observatory_id":Uuid::new_v4(),"observatory_name":"My constellation","pairing_id":Uuid::new_v4(),"expires_at":0})).unwrap()
}
#[test]
fn credential_and_identity_storage_survive_reopen_without_overwriting_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().to_owned()).unwrap();
    let secret = store.secret().unwrap();
    assert_eq!(secret.len(), 64);
    assert_eq!(
        Store::open(temp.path().to_owned())
            .unwrap()
            .secret()
            .unwrap(),
        secret
    );
    store
        .save("state.json", &json!({"machine":"original"}))
        .unwrap();
    store
        .save("state.json", &json!({"machine":"same","revision":2}))
        .unwrap();
    assert_eq!(
        store
            .read::<serde_json::Value>("state.json")
            .unwrap()
            .unwrap()["revision"],
        2
    );
    assert!(store.save("../secret", &json!({})).is_err());
    std::fs::write(temp.path().join("credential"), "broken").unwrap();
    assert!(store.secret().is_err());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("credential")).unwrap(),
        "broken"
    );
}
#[cfg(unix)]
#[test]
fn files_are_private_and_symlinks_are_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("private")).unwrap();
    store.secret().unwrap();
    assert_eq!(
        std::fs::metadata(&store.home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(store.home.join("credential"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    symlink(
        temp.path().join("outside"),
        store.home.join("redirect.json"),
    )
    .unwrap();
    assert!(store.save("redirect.json", &json!({})).is_err());
}
#[test]
fn bootstrap_requires_one_https_origin_and_public_identity() {
    let b = bootstrap();
    assert!(b.validate().is_ok());
    for url in [
        "http://localhost",
        "https://user:password@example.com",
        "https://example.com/remote/path",
        "https://example.com/?credential=secret",
        "https://example.com/#ignored",
    ] {
        let mut other = b.clone();
        other.observatory_url = url.into();
        assert!(other.validate().is_err(), "{url}");
    }
}
#[test]
fn startup_templates_escape_paths_and_do_not_contain_remote_commands() {
    let exe = Path::new("/Users/A Person/a&b/helper");
    let home = Path::new("/Users/A Person/a&b/own % folder");
    let systemd = platform::systemd_unit(exe, home).unwrap();
    assert!(systemd.contains("own %% folder"));
    assert!(systemd.contains(" --home "));
    assert!(systemd.contains(" run\n"));
    let plist = platform::launch_agent("safe-label", exe, home).unwrap();
    assert!(plist.contains("a&amp;b"));
    assert!(!plist.contains("shell"));
    let xml = platform::scheduled_task(
        Path::new("C:\\Users\\A Person\\helper.exe"),
        Path::new("C:\\Users\\A Person\\state"),
    )
    .unwrap();
    assert!(xml.contains("LeastPrivilege"));
    assert!(xml.contains("InteractiveToken"));
    assert!(!xml.contains("Password"));
    assert!(platform::systemd_unit(Path::new("/bad\ncommand"), home).is_err());
}
#[test]
fn mac_app_finds_bootstrap_outside_app_without_embedding_a_code() {
    let temp = tempfile::tempdir().unwrap();
    let exe = temp
        .path()
        .join("Constellation Computer.app/Contents/MacOS/constellation-machine-helper");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    let expected = temp.path().join("observatory-pairing.json");
    std::fs::write(&expected, "{}").unwrap();
    assert_eq!(platform::bootstrap_path(&exe), Some(expected));
}
#[tokio::test]
async fn local_capability_host_origin_and_private_response_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let app = Arc::new(
        App::open(
            Store::open(temp.path().to_owned()).unwrap(),
            bootstrap(),
            "http://127.0.0.1:43123".into(),
        )
        .unwrap(),
    );
    let token = app.token.clone();
    let router = app::router(app);
    for (host, origin, auth, status) in [
        (
            "evil.example",
            None,
            token.as_str(),
            StatusCode::BAD_REQUEST,
        ),
        (
            "127.0.0.1:43123",
            Some("https://evil.example"),
            token.as_str(),
            StatusCode::BAD_REQUEST,
        ),
        ("127.0.0.1:43123", None, "wrong", StatusCode::BAD_REQUEST),
        ("127.0.0.1:43123", None, token.as_str(), StatusCode::OK),
    ] {
        let mut req = Request::builder()
            .uri("/api/status")
            .header("host", host)
            .header("authorization", format!("Bearer {auth}"));
        if let Some(origin) = origin {
            req = req.header("origin", origin)
        }
        let response = router
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        if status == StatusCode::OK {
            let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
            let body = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(!body.contains("machine_credential"));
            assert!(!body.contains(&token));
            assert!(
                !body.contains(&std::fs::read_to_string(temp.path().join("credential")).unwrap())
            );
        }
    }
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/pair")
                .header("host", "127.0.0.1:43123")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"code":"ABCD-EFGH","startup":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(temp.path().join("state.json")).unwrap()
        )
        .unwrap()["pending"]
            .is_null()
    );
}

#[test]
fn windows_task_ownership_requires_exact_command_and_home() {
    let exe = Path::new("C:\\Users\\A\\helper.exe");
    let home = Path::new("C:\\Users\\A\\my & own state");
    let sid = "S-1-5-21-100-200-300-1001";
    let text = platform::scheduled_task(exe, home)
        .unwrap()
        .replace("<LogonType>", &format!("<UserId>{sid}</UserId><LogonType>"));
    assert!(platform::task_matches(&text, exe, home, sid).unwrap());
    assert!(!platform::task_matches(&text.replace(sid, "S-1-5-18"), exe, home, sid).unwrap());
    assert!(
        !platform::task_matches(
            &text.replace("LeastPrivilege", "HighestAvailable"),
            exe,
            home,
            sid
        )
        .unwrap()
    );
    assert!(
        !platform::task_matches(
            &text.replace("InteractiveToken", "ServiceAccount"),
            exe,
            home,
            sid
        )
        .unwrap()
    );

    assert!(!platform::task_matches(&text, Path::new("C:\\foreign.exe"), home, sid).unwrap());
    assert!(!platform::task_matches(&text, exe, Path::new("C:\\another home"), sid).unwrap());
    assert!(
        !platform::task_matches(
            &text.replace(
                "</Actions>",
                "<Exec><Command>other.exe</Command></Exec></Actions>"
            ),
            exe,
            home,
            sid
        )
        .unwrap()
    );
}

#[test]
fn windows_task_query_may_omit_only_the_default_run_level() {
    // Captured Windows Server 2025 schtasks /Query XML from the native smoke
    // test, with generated identities/paths replaced and re-encoded as UTF-8.
    let text = include_str!("fixtures/windows-task-query.xml");
    let home = Path::new(r"C:\Users\Example\AppData\Local\Temp\fixture\helper with spaces");
    let exe = Path::new(
        r"C:\Users\Example\AppData\Local\Temp\fixture\helper with spaces\bin\constellation-machine-helper.exe",
    );
    let sid = "S-1-5-21-100-200-300-1001";
    assert!(platform::task_matches(text, exe, home, sid).unwrap());
    assert!(!platform::task_matches(text, exe, home, "S-1-5-18").unwrap());
    assert!(
        !platform::task_matches(
            &text.replace("InteractiveToken", "ServiceAccount"),
            exe,
            home,
            sid,
        )
        .unwrap()
    );
    for invalid in [
        "<RunLevel>HighestAvailable</RunLevel>",
        "<RunLevel></RunLevel>",
        "<RunLevel />",
        "<RunLevel>LeastPrivilege",
        "<RunLevel><nested>LeastPrivilege</nested></RunLevel>",
        "<RunLevel>LeastPrivilege</RunLevel><RunLevel>LeastPrivilege</RunLevel>",
        "<RunLevel>LeastPrivilege</RunLevel><RunLevel />",
    ] {
        let altered = text.replace("</Principal>", &format!("{invalid}</Principal>"));
        assert!(
            !platform::task_matches(&altered, exe, home, sid).unwrap(),
            "must refuse {invalid}",
        );
    }
}

#[test]
fn moved_mac_app_keeps_public_bootstrap_inside_resources() {
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("translocated/Constellation Computer.app");
    let executable = app.join("Contents/MacOS/constellation-machine-helper");
    let config = app.join("Contents/Resources/observatory-pairing.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(&config, "{}").unwrap();
    assert_eq!(platform::bootstrap_path(&executable), Some(config));
}
