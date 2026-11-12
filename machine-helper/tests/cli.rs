use serde_json::{Value, json};
use std::{
    fs,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct RunningHelper(Child);
impl Drop for RunningHelper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn explicit_home_starts_without_default_home_environment() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("explicit helper home");
    let bootstrap = temp.path().join("observatory-pairing.json");
    fs::write(
        &bootstrap,
        serde_json::to_vec(&json!({
            "schema":1,
            "observatory_url":"https://observatory.invalid",
            "observatory_id":uuid::Uuid::new_v4(),
            "observatory_name":"Disposable Observatory",
            "pairing_id":uuid::Uuid::new_v4(),
            "expires_at":0,
        }))
        .unwrap(),
    )
    .unwrap();
    let stderr = temp.path().join("helper.stderr");
    let mut helper = RunningHelper(
        Command::new(env!("CARGO_BIN_EXE_constellation-machine-helper"))
            .arg("--home")
            .arg(&home)
            .arg("--bootstrap")
            .arg(&bootstrap)
            .args(["--no-browser", "setup"])
            .env_remove("HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("USERPROFILE")
            .env_remove("LOCALAPPDATA")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap(),
    );
    // No code is submitted: this opens only a disposable loopback setup page.
    // No remote request or startup registration is needed to prove CLI setup.
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let status: Value = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(status) = helper.0.try_wait().unwrap() {
                panic!(
                    "helper exited before local setup: {status}; {}",
                    fs::read_to_string(&stderr).unwrap(),
                );
            }
            if let Ok(bytes) = fs::read(home.join("local-session.json")) {
                let session: Value = serde_json::from_slice(&bytes).unwrap();
                if let Ok(response) = client
                    .get(format!(
                        "{}/api/status",
                        session["origin"].as_str().unwrap()
                    ))
                    .bearer_auth(session["token"].as_str().unwrap())
                    .send()
                    .await
                    && response.status().is_success()
                {
                    break response.json().await.unwrap();
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("helper did not serve local setup within 10 seconds");
    assert_eq!(status["state"], "unpaired");
    assert_eq!(status["startup_enabled"], false);
    assert_eq!(status["pending"], false);
    assert!(status["host_id"].is_null());
    assert!(home.join("observatory.json").is_file());
}
