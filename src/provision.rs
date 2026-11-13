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

// Existing SSH recipes execute on POSIX hosts. Interpret their paths in that
// host namespace even when Observatory itself runs on Windows.
fn posix_folder(value: &str) -> Option<Vec<&str>> {
    if !value.starts_with('/') || value.starts_with("//") || value.contains('\0') {
        return None;
    }
    let parts: Vec<_> = value
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.is_empty() || parts.contains(&"..") {
        None
    } else {
        Some(parts)
    }
}
fn valid_root(value: &str, remote: bool) -> bool {
    if remote {
        posix_folder(value).is_some()
    } else {
        let path = Path::new(value);
        path.is_absolute()
            && path.parent().is_some()
            && !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
    }
}
fn within_root(value: &str, root: &str, remote: bool) -> bool {
    if remote {
        match (posix_folder(value), posix_folder(root)) {
            (Some(path), Some(root)) => path.len() > root.len() && path.starts_with(&root),
            _ => false,
        }
    } else {
        let path = Path::new(value);
        valid_root(value, false) && path.starts_with(root) && path != Path::new(root)
    }
}

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
            .is_none_or(|s| !valid_root(s, transport == "ssh"))
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
    if host["transport"] == "paired" {
        return crate::paired_install::plan(state, &host, body);
    }
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
    let local_folder;
    let folder = if host["native_local"] == true && recipe["id"] == "bundled-instance" {
        local_folder =
            crate::local_install::folder(&host, body).map_err(|e| ApiError::bad(e.to_string()))?;
        local_folder
            .to_str()
            .ok_or_else(|| ApiError::bad("The installation folder is not supported"))?
    } else {
        body["home"].as_str().unwrap_or("")
    };
    if !host["install_roots"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| within_root(folder, r.as_str().unwrap(), host["transport"] == "ssh"))
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
stopped=False
def stop():
 global stopped
 if stopped:return
 try:os.killpg(p.pid,signal.SIGKILL)
 except ProcessLookupError:pass
 stopped=True
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
            if code == 0 && plan["recipe_id"] == "bundled-instance" {
                match crate::local_install::attach(&state, &job).await {
                    Ok(endpoint) => {
                        job["endpoint_id"] = endpoint["id"].clone();
                        job["note"] = json!(
                            "Your agent is installed and connected. Choose its model access and the places it can join."
                        );
                    }
                    Err(error) => {
                        job["connection_pending"] = json!(true);
                        job["note"] = json!(format!(
                            "The agent was installed, but its connection needs attention: {error}. Its installation has been preserved."
                        ));
                    }
                }
            }
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
    #[cfg(not(windows))]
    let mut child = cmd
        .spawn()
        .map_err(|_| ApiError::bad("Could not start the host installer"))?;
    #[cfg(unix)]
    let pid = child.id().unwrap_or(0) as i32;
    #[cfg(windows)]
    let (mut child, process_job) = crate::process_tree::ProcessJob::spawn(&mut cmd)
        .await
        .map_err(|_| {
            ApiError::bad(
                "Could not establish installer process cleanup; inspect the host before retrying",
            )
        })?;
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
    #[cfg(windows)]
    drop(process_job);
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
    if crate::desktop_update::busy(&state) {
        return Err(ApiError::busy());
    }
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
    if p["transport"] != "paired" {
        tokio::spawn(execute(state, request.into(), p));
    }
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::Write;
    #[test]
    fn remote_roots_use_posix_paths_independently_of_observatory_os() {
        assert!(valid_root("/srv/agents", true));
        assert!(within_root("/srv/agents/one", "/srv/agents", true));
        for folder in [
            "/",
            "/srv/agents",
            "/srv/agents-other/one",
            "/srv/agents/../elsewhere",
            "C:\\agents\\one",
            "relative",
        ] {
            assert!(!within_root(folder, "/srv/agents", true), "{folder}");
        }
        assert!(!valid_root("/", true));
        assert!(!valid_root("/srv/../etc", true));
        let temp = tempfile::tempdir().unwrap();
        assert!(valid_root(temp.path().to_str().unwrap(), false));
        assert!(within_root(
            temp.path().join("agent").to_str().unwrap(),
            temp.path().to_str().unwrap(),
            false
        ));
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn local_installer_completion_and_timeout_stop_windows_descendants() {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, WAIT_OBJECT_0},
            System::Threading::{
                OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
                WaitForSingleObject,
            },
        };
        for timeout in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let c:config::Config=serde_json::from_value(json!({"id":uuid::Uuid::new_v4(),"owner":"fixture","bind":"127.0.0.1","port":0,"public_url":"","pam_service":"unused","tls_cert":"","tls_key":"","tls_ca":null})).unwrap();
            c.save(home.path()).unwrap();
            let state = AppState::open(home.path().into()).unwrap();
            let host = state
                .save("hosts", json!({"id":uuid::Uuid::new_v4()}), Some(0))
                .unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            state
                .save("jobs", json!({"id":id,"status":"queued"}), Some(0))
                .unwrap();
            let pids = home.path().join("installer-pids.json");
            let script = r#"
import json, os, subprocess, sys, time
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'])
with open(sys.argv[1],'w') as out: json.dump([os.getpid(),child.pid],out)
print('fixture started',flush=True)
time.sleep(60 if sys.argv[2]=='timeout' else 0.1)
"#;
            let plan = json!({"host_id":host["id"],"host_revision":host["revision"],"transport":"local","argv":["python","-c",script,pids,if timeout {"timeout"} else {"finish"}],"timeout_seconds":0});
            let result = execute_inner(&state, &id, &plan).await;
            let ids: Value = serde_json::from_slice(&std::fs::read(pids).unwrap()).unwrap();
            let mut alive = vec![];
            for pid in ids.as_array().unwrap() {
                let pid = pid.as_u64().unwrap() as u32;
                // SAFETY: only fixture-owned PIDs; close each acquired handle.
                unsafe {
                    let process = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid);
                    if !process.is_null() {
                        if WaitForSingleObject(process, 3000) != WAIT_OBJECT_0 {
                            alive.push(pid);
                            TerminateProcess(process, 1);
                            WaitForSingleObject(process, 3000);
                        }
                        CloseHandle(process);
                    } else {
                        assert_eq!(
                            std::io::Error::last_os_error().raw_os_error(),
                            Some(87),
                            "Cannot inspect fixture process"
                        );
                    }
                }
            }
            assert!(
                alive.is_empty(),
                "Installer descendants survived: {alive:?}"
            );
            assert_eq!(result.is_err(), timeout);
            assert!(
                std::fs::read_to_string(state.home.join("jobs").join(format!("{id}.stdout")))
                    .unwrap()
                    .contains("fixture started")
            );
        }
    }
    #[cfg(unix)]
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
            // A second signal after the parent is reaped can target a vanished
            // or reused process group (EPERM on macOS). Only successful cleanup
            // is memoized; a genuine first-attempt failure must still surface.
            let remote = format!(
                r#"import os
original_killpg=os.killpg
cleanup_calls=0
def once(group, sig):
 global cleanup_calls
 cleanup_calls+=1
 if cleanup_calls>1:raise PermissionError('cleanup repeated after settlement')
 return original_killpg(group,sig)
os.killpg=once
{REMOTE}"#
            );
            let mut process = std::process::Command::new("python3")
                .args(["-c", &remote])
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
            assert!(
                [Some(124), Some(125)].contains(&output.status.code()),
                "Remote fixture exited {:?}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            std::thread::sleep(Duration::from_millis(800));
            assert!(
                !marker.exists(),
                "Remote descendant survived installer termination"
            );
        }
    }
}
