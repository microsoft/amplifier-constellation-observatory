//! Host access is a separate grant, never inferred from an agent management URL.
use crate::{
    auth, config,
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

pub fn validate_host(host: &Value) -> ApiResult<()> {
    if host["label"]
        .as_str()
        .is_none_or(|s| s.is_empty() || s.len() > 160)
    {
        return Err(ApiError::bad("Name this host"));
    }
    let transport = host["transport"].as_str().unwrap_or("");
    if !["local", "ssh"].contains(&transport) {
        return Err(ApiError::bad("Choose this machine or an SSH host"));
    }
    if transport == "ssh"
        && host["address"].as_str().is_none_or(|s| {
            s.is_empty()
                || s.starts_with('-')
                || s.len() > 250
                || !s
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b".@:_-".contains(&c))
        })
    {
        return Err(ApiError::bad("Use an SSH host name or configured alias"));
    }
    let roots = host["install_roots"]
        .as_array()
        .ok_or_else(|| ApiError::bad("Choose allowed installation folders"))?;
    if roots.iter().any(|x| {
        x.as_str()
            .is_none_or(|s| !Path::new(s).is_absolute() || s == "/" || s.contains(".."))
    }) {
        return Err(ApiError::bad("Use specific absolute installation roots"));
    }
    let recipes = host["recipes"]
        .as_array()
        .ok_or_else(|| ApiError::bad("Configure at least one installer recipe"))?;
    if recipes.len() > 30 {
        return Err(ApiError::bad("Configure at most 30 installer recipes"));
    }
    if !recipes.is_empty() && roots.is_empty() {
        return Err(ApiError::bad(
            "Choose an installation folder before enabling an installer",
        ));
    }
    for r in recipes {
        if !config::safe_name(r["id"].as_str().unwrap_or(""))
            || r["version"]
                .as_str()
                .is_none_or(|s| s.is_empty() || s.len() > 120)
            || r["argv"].as_array().is_none_or(|a| {
                a.is_empty()
                    || a.len() > 100
                    || a.iter().any(|x| x.as_str().is_none_or(|s| s.len() > 8000))
            })
        {
            return Err(ApiError::bad(
                "Each recipe needs an ID, pinned version and argument array",
            ));
        }
    }
    Ok(())
}
pub fn plan(state: &AppState, body: &Value) -> ApiResult<Value> {
    let host = state.read("hosts", body["host_id"].as_str().unwrap_or(""))?;
    validate_host(&host)?;
    let recipe = host["recipes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == body["recipe_id"])
        .ok_or_else(|| ApiError::bad("Choose an installed host recipe"))?;
    let name = body["name"].as_str().unwrap_or("").trim();
    if name.is_empty() || name.len() > 120 || name.contains(['\n', '\r', '\0']) {
        return Err(ApiError::bad("Give the new installation a short name"));
    }
    let folder = body["home"].as_str().unwrap_or("");
    let path = Path::new(folder);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        || !host["install_roots"].as_array().unwrap().iter().any(|r| {
            path.starts_with(r.as_str().unwrap()) && path != Path::new(r.as_str().unwrap())
        })
    {
        return Err(ApiError::bad(
            "Choose a new folder inside this host's allowed installation roots",
        ));
    }
    let port = body["port"].as_u64().unwrap_or(0);
    if port > 65535 {
        return Err(ApiError::bad("Invalid port"));
    }
    let version = recipe["version"].as_str().unwrap();
    let argv: Vec<String> = recipe["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            a.as_str()
                .unwrap()
                .replace("{name}", name)
                .replace("{home}", folder)
                .replace("{port}", &port.to_string())
                .replace("{version}", version)
        })
        .collect();
    if argv[0].starts_with('-') || argv[0].is_empty() {
        return Err(ApiError::bad("Invalid installer executable"));
    }
    let mut result = json!({"contract":"agent-observatory.host-plan/1","host_id":host["id"],"host_revision":host["revision"],"recipe_id":recipe["id"],"version":version,"name":name,"home":folder,"port":port,"transport":host["transport"],"address":host["address"],"argv":argv,"cwd":recipe["cwd"],"timeout_seconds":recipe["timeout_seconds"].as_u64().unwrap_or(1800).clamp(30,3600),"authority":"explicit host provisioning; separate from agent management"});
    result["plan_digest"] = json!(auth::digest(&result.to_string()));
    Ok(result)
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
// JSON on stdin; no user-provided shell command is evaluated by the remote host.
const REMOTE: &str = r#"import json,os,select,signal,subprocess,sys,time
r=json.loads(sys.stdin.readline())
p=subprocess.Popen(r['argv'],cwd=r.get('cwd') or None,stdin=subprocess.DEVNULL,start_new_session=True)
disconnected=False
def stop():
 try:os.killpg(p.pid,signal.SIGKILL)
 except ProcessLookupError:pass
def interrupted(signum,frame):
 global disconnected
 disconnected=True;stop()
for sig in [signal.SIGHUP,signal.SIGTERM,signal.SIGINT]:signal.signal(sig,interrupted)
deadline=time.monotonic()+r['timeout_seconds']
code=125
try:
 while p.poll() is None:
  remaining=deadline-time.monotonic()
  if remaining<=0:
   stop();p.wait();code=124
   print('Installer timed out; inspect installation before retrying.',file=sys.stderr)
   break
  ready,_,_=select.select([sys.stdin],[],[],min(.1,remaining))
  if ready and not os.read(sys.stdin.fileno(),4096):disconnected=True;stop()
  if disconnected:p.wait();code=125;break
 else:code=p.returncode
finally:stop()
sys.exit(code)
"#;
pub async fn execute(state: Arc<AppState>, job_id: String, plan: Value) {
    let result = execute_inner(&state, &job_id, &plan).await;
    let Ok(mut job) = state.read("jobs", &job_id) else {
        return;
    };
    match result {
        Ok(code) => {
            job["status"] = json!(if code == 0 { "completed" } else { "failed" });
            job["exit_code"] = json!(code);
            job["note"] = json!(if code == 0 {
                "Installer completed. Inspect the returned endpoint before claiming the agent is ready for work."
            } else {
                "Installation failed. Inspect the preserved result before retrying."
            });
        }
        Err(e) => {
            job["status"] = json!("interrupted");
            job["note"] = json!(e.1);
        }
    }
    job["completed_at"] = json!(now_ms());
    let _ = state.save("jobs", job.clone(), None);
    let _ = state.audit(
        "installation.finished",
        Some(&job_id),
        json!({"status":job["status"],"host_id":plan["host_id"]}),
    );
}
async fn execute_inner(state: &AppState, id: &str, plan: &Value) -> ApiResult<i32> {
    let _permit = state
        .operation_gate
        .acquire()
        .await
        .map_err(|_| ApiError::busy())?;
    let host = state.read("hosts", plan["host_id"].as_str().unwrap_or(""))?;
    if host["revision"] != plan["host_revision"] {
        return Err(ApiError::bad(
            "Host settings changed before installation; create a new plan",
        ));
    }
    let mut job = state.read("jobs", id)?;
    job["status"] = json!("running");
    state.save("jobs", job, None)?;
    let argv = plan["argv"].as_array().unwrap();
    let mut cmd = if plan["transport"] == "ssh" {
        let mut c = Command::new("ssh");
        c.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "--",
            plan["address"].as_str().unwrap(),
        ]);
        c.arg(format!("python3 -c {}", quote(REMOTE)));
        c
    } else {
        let mut c = Command::new(argv[0].as_str().unwrap());
        for a in &argv[1..] {
            c.arg(a.as_str().unwrap());
        }
        if let Some(cwd) = plan["cwd"].as_str() {
            c.current_dir(cwd);
        }
        c
    };
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|_| ApiError::bad("Could not start the host installer"))?;
    #[cfg(unix)]
    let pid = child.id().unwrap_or(0) as i32;
    // Keep the remote control stream open for the whole operation. EOF means the
    // initiating process/SSH channel disappeared and the remote group must stop.
    let mut remote_control = None;
    if plan["transport"] == "ssh" {
        let mut bytes = serde_json::to_vec(plan).unwrap();
        bytes.push(b'\n');
        if let Some(mut input) = child.stdin.take() {
            input.write_all(&bytes).await?;
            input.flush().await?;
            remote_control = Some(input);
        }
    } else {
        drop(child.stdin.take());
    }
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    async fn bounded<R: tokio::io::AsyncRead + Unpin>(mut r: R) -> std::io::Result<Vec<u8>> {
        let mut out = vec![];
        let mut b = [0; 8192];
        loop {
            let n = r.read(&mut b).await?;
            if n == 0 {
                break;
            }
            if out.len() < 1024 * 1024 {
                out.extend_from_slice(&b[..n.min(1024 * 1024 - out.len())]);
            }
        }
        Ok(out)
    }
    let mut output = tokio::spawn(bounded(stdout));
    let mut errors = tokio::spawn(bounded(stderr));
    let status = tokio::time::timeout(
        Duration::from_secs(plan["timeout_seconds"].as_u64().unwrap() + 10),
        child.wait(),
    )
    .await;
    if status.is_err() {
        #[cfg(unix)]
        if pid > 0 {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = child.kill().await;
    }
    drop(remote_control);
    // Installers own their foreground process tree. A successful parent exit
    // must not leave orphan helpers holding output streams or altering files.
    #[cfg(unix)]
    if pid > 0 {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let out = tokio::time::timeout(Duration::from_secs(3), &mut output).await;
    let err = tokio::time::timeout(Duration::from_secs(3), &mut errors).await;
    let streams_complete = out.is_ok() && err.is_ok();
    output.abort();
    errors.abort();
    let out = out
        .ok()
        .and_then(Result::ok)
        .transpose()?
        .unwrap_or_default();
    let err = err
        .ok()
        .and_then(Result::ok)
        .transpose()?
        .unwrap_or_default();
    config::private_write(&state.home.join("jobs").join(format!("{id}.stdout")), &out)?;
    config::private_write(&state.home.join("jobs").join(format!("{id}.stderr")), &err)?;
    if !streams_complete {
        return Err(ApiError::bad(
            "Installer left output streams open; inspect the host before retrying",
        ));
    }
    status
        .map_err(|_| ApiError::bad("Installer timed out; inspect the host before retrying"))?
        .map(|s| s.code().unwrap_or(-1))
        .map_err(|_| ApiError::bad("Installer status could not be read"))
}
pub fn start(state: Arc<AppState>, body: &Value, actor: &str) -> ApiResult<Value> {
    let gate = state.provision_gate.lock().unwrap();
    let request = body["request_id"]
        .as_str()
        .ok_or_else(|| ApiError::bad("Supply a unique request ID"))?;
    if uuid::Uuid::parse_str(request).is_err() {
        return Err(ApiError::bad("Use a UUID request ID"));
    }
    let hash = auth::digest(&body.to_string());
    if let Ok(old) = state.read("jobs", request) {
        if old["request_hash"] != hash {
            return Err(ApiError::bad(
                "This request ID was already used for different inputs",
            ));
        }
        return Ok(old);
    }
    let p = plan(&state, body)?;
    if body["plan_digest"] != p["plan_digest"] {
        return Err(ApiError::bad(
            "Inspect a current installation plan before applying it",
        ));
    }
    if state.list("jobs")?.iter().any(|j| {
        j["plan"]["host_id"] == p["host_id"]
            && ["queued", "running"].contains(&j["status"].as_str().unwrap_or(""))
    }) {
        return Err(ApiError::busy());
    }
    let job=state.save("jobs",json!({"id":request,"kind":"provision","status":"queued","plan":p,"request_hash":hash,"actor":actor,"note":"Waiting for an installation slot"}),Some(0))?;
    state.audit(
        "installation.queued",
        Some(request),
        json!({"host_id":p["host_id"],"name":p["name"],"actor":actor}),
    )?;
    drop(gate);
    tokio::spawn(execute(state, request.into(), p));
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn remote_timeout_and_connection_loss_stop_descendants() {
        for disconnect in [false, true] {
            let folder = tempfile::tempdir().unwrap();
            let marker = folder.path().join("completed");
            let work = format!(
                "import subprocess,sys,time;subprocess.Popen([sys.executable,'-c',{}]);time.sleep(10)",
                serde_json::to_string(&format!(
                    "import time,pathlib;time.sleep(.7);pathlib.Path({}).write_text('unexpected')",
                    serde_json::to_string(marker.to_str().unwrap()).unwrap()
                ))
                .unwrap()
            );
            let mut process = std::process::Command::new("python3")
                .args(["-c", REMOTE])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut input = process.stdin.take().unwrap();
            writeln!(
                input,
                "{}",
                json!({"argv":["python3","-c",work],"timeout_seconds":0.15})
            )
            .unwrap();
            if disconnect {
                drop(input);
            } else {
                std::thread::sleep(Duration::from_millis(400));
                drop(input);
            }
            let output = process.wait_with_output().unwrap();
            assert!([Some(124), Some(125)].contains(&output.status.code()));
            std::thread::sleep(Duration::from_millis(800));
            assert!(
                !marker.exists(),
                "Remote descendant survived installer termination"
            );
        }
    }
}
