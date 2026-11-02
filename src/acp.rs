//! Ordinary stdio ACP; management is intentionally not involved in negotiation.
use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

pub fn validate(v: &Value) -> ApiResult<()> {
    if v["transport"].as_str().unwrap_or("stdio") != "stdio" {
        return Err(ApiError::bad(
            "Use a standard ACP stdio command; remote endpoints can supply an explicit attach command",
        ));
    }
    if v["command"]
        .as_str()
        .is_none_or(|s| s.is_empty() || s.len() > 4096)
        || v["cwd"]
            .as_str()
            .is_none_or(|s| !std::path::Path::new(s).is_absolute())
    {
        return Err(ApiError::bad(
            "Supply an ACP executable and absolute working folder",
        ));
    }
    if v.get("session_cwd").is_some_and(|p| {
        p.as_str()
            .is_none_or(|p| !std::path::Path::new(p).is_absolute())
    }) {
        return Err(ApiError::bad("The agent session folder must be absolute"));
    }
    if v.get("args").is_some_and(|a| {
        a.as_array().is_none_or(|xs| {
            xs.len() > 100 || xs.iter().any(|s| s.as_str().is_none_or(|x| x.len() > 8000))
        })
    }) {
        return Err(ApiError::bad("Arguments must be a bounded text array"));
    }
    if v.get("secret_env").is_some_and(|e| {
        e.as_object().is_none_or(|m| {
            m.iter().any(|(k, v)| {
                !crate::config::safe_name(k) || !v.as_str().is_some_and(crate::config::safe_name)
            })
        })
    }) {
        return Err(ApiError::bad(
            "Use named secret references for environment credentials",
        ));
    }
    Ok(())
}
struct Client {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: u64,
    #[cfg(unix)]
    pid: i32,
}
impl Drop for Client {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.pid > 0 {
            unsafe {
                libc::kill(-self.pid, libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
    }
}
impl Client {
    async fn start(state: &AppState, v: &Value) -> Result<Self> {
        validate(v).map_err(|e| anyhow::anyhow!(e.1))?;
        let mut cmd = Command::new(v["command"].as_str().unwrap());
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.current_dir(v["cwd"].as_str().unwrap())
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for k in [
            "PATH",
            "HOME",
            "USER",
            "LANG",
            "SYSTEMROOT",
            "WINDIR",
            "TEMP",
            "TMP",
            "SSH_AUTH_SOCK",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
        ] {
            if let Ok(x) = std::env::var(k) {
                cmd.env(k, x);
            }
        }
        if let Some(args) = v["args"].as_array() {
            for a in args {
                cmd.arg(a.as_str().unwrap());
            }
        }
        if let Some(env) = v["env"].as_object() {
            for (k, value) in env {
                ensure!(crate::config::safe_name(k), "Invalid environment name");
                cmd.env(
                    k,
                    value.as_str().context("Environment values must be text")?,
                );
            }
        }
        if let Some(env) = v["secret_env"].as_object() {
            for (k, value) in env {
                cmd.env(
                    k,
                    state
                        .secret(value.as_str().unwrap())
                        .map_err(|e| anyhow::anyhow!(e.1))?,
                );
            }
        }
        let mut child = cmd.spawn()?;
        #[cfg(unix)]
        let pid = child.id().context("ACP process exited")? as i32;
        Ok(Self {
            input: child.stdin.take().context("Missing ACP input")?,
            output: BufReader::new(child.stdout.take().context("Missing ACP output")?),
            child,
            next: 0,
            #[cfg(unix)]
            pid,
        })
    }
    async fn send(&mut self, v: Value) -> Result<()> {
        self.input.write_all(format!("{v}\n").as_bytes()).await?;
        self.input.flush().await?;
        Ok(())
    }
    async fn read(&mut self) -> Result<Value> {
        let mut data = Vec::new();
        loop {
            let b = self.output.fill_buf().await?;
            ensure!(!b.is_empty(), "ACP endpoint disconnected");
            let n = b
                .iter()
                .position(|x| *x == b'\n')
                .map(|n| n + 1)
                .unwrap_or(b.len());
            ensure!(data.len() + n <= 24 * 1024 * 1024, "ACP message too large");
            data.extend_from_slice(&b[..n]);
            self.output.consume(n);
            if data.last() == Some(&b'\n') {
                break;
            }
        }
        let v: Value = serde_json::from_slice(&data)?;
        ensure!(v["jsonrpc"] == "2.0", "Invalid ACP frame");
        Ok(v)
    }
    async fn cancel(&mut self, id: u64, sid: &str) {
        let _ = self
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":sid}}))
            .await;
        // Give a remote attach process time to forward cancellation and receive the stop result.
        let _=tokio::time::timeout(Duration::from_secs(3),async {
            loop {
                let v=self.read().await?;
                if v.get("method").is_none() && v["id"]==id { return Ok::<_,anyhow::Error>(()); }
                if v["method"]=="session/request_permission" && v.get("id").is_some() {
                    self.send(json!({"jsonrpc":"2.0","id":v["id"],"result":crate::permissions::cancelled()})).await?;
                }
            }
        }).await;
    }
    async fn request(
        &mut self,
        method: &str,
        params: Value,
        updates: &mut Vec<Value>,
        interaction: Option<&Interaction<'_>>,
    ) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        let mut cancellation = interaction.and_then(|ctx| {
            ctx.state
                .cancellations
                .lock()
                .unwrap()
                .get(ctx.room)
                .map(|(_, tx)| tx.subscribe())
        });
        let mut update_bytes = 0usize;
        loop {
            let v = tokio::select! {
                value=self.read()=>value?,
                _=cancel_signal(&mut cancellation)=>{
                    if let Some(ctx)=interaction { self.cancel(id,ctx.sid).await; }
                    anyhow::bail!("Turn cancelled");
                }
                _=turn_deadline(interaction)=>{
                    if let Some(ctx)=interaction { self.cancel(id,ctx.sid).await; }
                    anyhow::bail!("Turn timed out");
                }
            };
            if v.get("method").is_none() && v["id"] == id {
                if let Some(error) = v.get("error") {
                    return Err(crate::acp_auth::rejected(error, method));
                }
                return Ok(v["result"].clone());
            }
            if v.get("id").is_some() && v.get("method").is_some() {
                if v["method"] == "session/request_permission" {
                    let result = if let Some(ctx) =
                        interaction.filter(|c| v["params"]["sessionId"] == c.sid)
                    {
                        match ctx.state.permissions.request(
                            ctx.room,
                            ctx.actor,
                            "Agent",
                            &v["params"],
                            Duration::from_secs(180).min(
                                ctx.deadline
                                    .saturating_duration_since(tokio::time::Instant::now()),
                            ),
                        ) {
                            Ok(request) => {
                                let _ = ctx.state.audit(
                                    "permission.requested",
                                    Some(ctx.room),
                                    json!({"actor":ctx.actor,"request_id":request.id}),
                                );
                                let outcome = tokio::select! {
                                    value=request.wait()=>Some(value),
                                    _=cancel_signal(&mut cancellation)=>None,
                                    _=self.child.wait()=>anyhow::bail!("Agent disconnected while awaiting a decision"),
                                };
                                if let Some(outcome) = outcome {
                                    outcome
                                } else {
                                    self.send(json!({"jsonrpc":"2.0","id":v["id"],"result":crate::permissions::cancelled()})).await?;
                                    self.cancel(id, ctx.sid).await;
                                    anyhow::bail!("Turn cancelled");
                                }
                            }
                            Err(_) => crate::permissions::cancelled(),
                        }
                    } else {
                        crate::permissions::cancelled()
                    };
                    self.send(json!({"jsonrpc":"2.0","id":v["id"],"result":result}))
                        .await?;
                } else {
                    self.send(json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32601,"message":"This observatory client does not offer that capability"}})).await?;
                }
            } else if v["method"] == "session/update" {
                if v["params"]["update"]["sessionUpdate"] == "agent_thought_chunk" {
                    continue;
                }
                update_bytes += v["params"].to_string().len();
                ensure!(
                    updates.len() < 2000 && update_bytes <= 48 * 1024 * 1024,
                    "Too many ACP updates"
                );
                if let Some(ctx) = interaction.filter(|c| v["params"]["sessionId"] == c.sid) {
                    progress(ctx.state, ctx.room, &v["params"]["update"])?;
                }
                updates.push(v["params"].clone());
            }
        }
    }
    async fn initialize(&mut self) -> Result<Value> {
        let init=self.request("initialize",json!({"protocolVersion":1,"clientInfo":{"name":"agent-observatory","version":env!("CARGO_PKG_VERSION")},"clientCapabilities":{}}),&mut vec![], None).await?;
        ensure!(init["protocolVersion"] == 1, "Unsupported ACP version");
        let mut init = init;
        init["_meta"]["io.github.bkrabach.agent-observatory"]["authentication"] =
            crate::acp_auth::authentication(&init);
        Ok(init)
    }
}
pub async fn probe(state: &AppState, config: &Value) -> ApiResult<Value> {
    let work = async {
        let mut client = Client::start(state, config).await?;
        client.initialize().await
    };
    tokio::time::timeout(Duration::from_secs(30), work)
        .await
        .map_err(|_| ApiError::bad("ACP connection timed out"))?
        .map_err(|error| {
            if crate::acp_auth::authentication_required(&error) {
                return ApiError::bad(error.to_string());
            }
            ApiError::bad(
                "ACP negotiation failed; check its executable, account, and working folder",
            )
        })
}
pub struct LiveSession {
    client: Client,
    init: Value,
    sid: String,
    used_at: i64,
}
pub fn evict_idle(state: &AppState) {
    state
        .acp_clients
        .lock()
        .unwrap()
        .retain(|_, client| match client.try_lock() {
            Ok(s) => crate::state::now_ms() - s.used_at < 15 * 60 * 1000,
            Err(_) => true,
        });
}
pub async fn prompt(
    state: &AppState,
    context: &str,
    config: &Value,
    prior: Option<&str>,
    text: &str,
    actor: &str,
) -> ApiResult<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    let work = async {
        let cached = state.acp_clients.lock().unwrap().get(context).cloned();
        let mut resumed = false;
        let live = if let Some(cached) = cached {
            cached
        } else {
            let mut setup_cancel = state
                .cancellations
                .lock()
                .unwrap()
                .get(context)
                .map(|(_, tx)| tx.subscribe());
            let mut client = Client::start(state, config).await?;
            let init = tokio::select! {
                _=cancel_signal(&mut setup_cancel)=>anyhow::bail!("Turn cancelled"),
                result=client.initialize()=>result?,
            };
            let mut updates = vec![];
            let sid = if let Some(prior) = prior {
                ensure!(
                    init["agentCapabilities"]["loadSession"] == true,
                    "This endpoint cannot restore a disconnected session. Its transcript is saved; start a new conversation."
                );
                tokio::select! {
                  _=cancel_signal(&mut setup_cancel)=>anyhow::bail!("Turn cancelled"),
                  result=client.request(
                        "session/load",
                        json!({"sessionId":prior,"cwd":config.get("session_cwd").unwrap_or(&config["cwd"]),"mcpServers":[]}),
                        &mut updates,
                        None,
                    )=>result?,
                };
                resumed = true;
                prior.to_owned()
            } else {
                let created = tokio::select! {
                  _=cancel_signal(&mut setup_cancel)=>anyhow::bail!("Turn cancelled"),
                  result=client.request(
                        "session/new",
                        json!({"cwd":config.get("session_cwd").unwrap_or(&config["cwd"]),"mcpServers":[]}),
                        &mut updates,
                        None,
                    )=>result?,
                };
                created["sessionId"]
                    .as_str()
                    .context("Missing ACP session ID")?
                    .to_owned()
            };
            let live = std::sync::Arc::new(tokio::sync::Mutex::new(LiveSession {
                client,
                init,
                sid,
                used_at: crate::state::now_ms(),
            }));
            let mut clients = state.acp_clients.lock().unwrap();
            clients.retain(|_, v| match v.try_lock() {
                Ok(s) => crate::state::now_ms() - s.used_at < 15 * 60 * 1000,
                Err(_) => true,
            });
            ensure!(
                clients.len() < 32,
                "Close an idle ACP conversation before opening another"
            );
            clients.insert(context.to_owned(), live.clone());
            live
        };
        let mut live = live.lock().await;
        let sid = live.sid.clone();
        let mut updates = vec![];
        let result = live
            .client
            .request(
                "session/prompt",
                json!({"sessionId":sid,"prompt":[{"type":"text","text":text}]}),
                &mut updates,
                Some(&Interaction {
                    state,
                    room: context,
                    actor,
                    sid: &sid,
                    deadline,
                }),
            )
            .await?;
        live.used_at = crate::state::now_ms();
        let reply = reply_text(&updates, &sid);
        Ok::<_, anyhow::Error>(
            json!({"session_id":sid,"resumed":resumed,"resumable":live.init["agentCapabilities"]["loadSession"]==true,"reply":reply,"media":reply_media(&updates,&sid),"stop_reason":result["stopReason"],"capabilities":live.init["agentCapabilities"],"updates":updates}),
        )
    };
    let result = match tokio::time::timeout(Duration::from_secs(605), work).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) if crate::acp_auth::authentication_required(&e) => {
            Err(ApiError::bad(e.to_string()))
        }
        Ok(Err(e)) if e.to_string().starts_with("This endpoint cannot restore") => {
            Err(ApiError::bad(
                "This endpoint cannot restore a disconnected session. Its transcript is saved; start a new conversation.",
            ))
        }
        Ok(Err(e)) if e.to_string() == "Turn timed out" => Err(ApiError::bad(
            "The ACP turn timed out; inspect the agent before retrying work",
        )),
        Ok(Err(_)) => Err(ApiError::bad(
            "The ACP turn could not complete. Check the agent connection before retrying.",
        )),
        Err(_) => Err(ApiError::bad(
            "The ACP turn timed out; inspect the agent before retrying work",
        )),
    };
    if result.is_err() {
        state.acp_clients.lock().unwrap().remove(context);
    }
    result
}

async fn cancel_signal(receiver: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    if let Some(rx) = receiver {
        if !*rx.borrow() {
            let _ = rx.changed().await;
        }
    } else {
        std::future::pending::<()>().await;
    }
}

struct Interaction<'a> {
    state: &'a AppState,
    room: &'a str,
    actor: &'a str,
    sid: &'a str,
    deadline: tokio::time::Instant,
}
async fn turn_deadline(ctx: Option<&Interaction<'_>>) {
    if let Some(ctx) = ctx {
        tokio::time::sleep_until(ctx.deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

fn progress(state: &AppState, room: &str, update: &Value) -> Result<()> {
    let mut all = state.progress.lock().unwrap();
    let p = all
        .entry(room.to_owned())
        .or_insert_with(|| json!({"text":"","tools":[],"plan":[],"revision":0}));
    match update["sessionUpdate"].as_str() {
        Some("agent_message_chunk") => {
            if let Some(m) = media_block(&update["content"]) {
                if !p["media"].is_array() {
                    p["media"] = json!([]);
                }
                let a = p["media"].as_array_mut().unwrap();
                ensure!(a.len() < 8, "Too many generated files");
                a.push(m);
            }
            if let Some(text) = update["content"]["text"].as_str() {
                let mut current = p["text"].as_str().unwrap_or("").to_owned();
                if p["after_tool"] == true && !current.is_empty() && !current.ends_with("\n\n") {
                    current.push_str("\n\n");
                }
                ensure!(
                    current.len() + text.len() <= 2 * 1024 * 1024,
                    "Response too large"
                );
                current.push_str(text);
                p["text"] = json!(current);
                p["after_tool"] = json!(false);
            }
        }
        Some("tool_call" | "tool_call_update") => {
            p["after_tool"] = json!(true);
            if let Some(id) = update["toolCallId"].as_str().filter(|id| id.len() <= 1024) {
                let tools = p["tools"].as_array_mut().unwrap();
                let index = tools.iter().position(|t| t["id"] == id);
                if index.is_none() {
                    if tools.len() >= 100 {
                        tools.remove(0);
                    }
                    tools.push(json!({"id":id}));
                }
                let index = index.unwrap_or(tools.len() - 1);
                for key in ["title", "kind", "status"] {
                    if let Some(value) = update[key].as_str() {
                        tools[index][key] = json!(value.chars().take(500).collect::<String>());
                    }
                }
            }
        }
        Some("plan") => {
            if let Some(entries) = update["entries"].as_array() {
                p["plan"]=json!(entries.iter().take(100).map(|e|json!({"content":e["content"].as_str().unwrap_or("").chars().take(1000).collect::<String>(),"status":e["status"].as_str().unwrap_or("pending")})).collect::<Vec<_>>());
            }
        }
        _ => return Ok(()), // Never expose agent_thought_chunk as public progress.
    }
    p["revision"] = json!(p["revision"].as_u64().unwrap_or(0) + 1);
    Ok(())
}

fn reply_text(updates: &[Value], sid: &str) -> String {
    let mut text = String::new();
    let mut after_tool = false;
    for update in updates.iter().filter(|u| u["sessionId"] == sid) {
        match update["update"]["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => {
                if let Some(chunk) = update["update"]["content"]["text"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                {
                    if after_tool && !text.is_empty() && !text.ends_with("\n\n") {
                        text.push_str("\n\n");
                    }
                    after_tool = false;
                    text.push_str(chunk);
                }
            }
            Some("tool_call" | "tool_call_update") => after_tool = true,
            _ => {}
        }
    }
    text
}

fn media_block(block: &Value) -> Option<Value> {
    use base64::Engine;
    let (mime, data) = match block["type"].as_str() {
        Some("image" | "audio") => (block["mimeType"].as_str()?, block["data"].as_str()?),
        Some("resource") => (
            block["resource"]["mimeType"]
                .as_str()
                .unwrap_or("application/octet-stream"),
            block["resource"]["blob"].as_str()?,
        ),
        _ => return None,
    };
    if data.len() > 16 * 1024 * 1024 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    if bytes.is_empty() {
        return None;
    }
    let mime = match mime {
        "image/png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => mime,
        "image/jpeg" if bytes.starts_with(b"\xff\xd8\xff") => mime,
        "image/gif" if bytes.starts_with(b"GIF8") => mime,
        "image/webp" if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => mime,
        "audio/wav" | "audio/mpeg" => mime,
        _ => "application/octet-stream",
    };
    Some(json!({"mime_type":mime,"data":data}))
}
fn reply_media(updates: &[Value], sid: &str) -> Vec<Value> {
    updates
        .iter()
        .filter(|u| u["sessionId"] == sid && u["update"]["sessionUpdate"] == "agent_message_chunk")
        .filter_map(|u| media_block(&u["update"]["content"]))
        .take(8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streamed_tokens_remain_contiguous_but_post_tool_answers_start_a_paragraph() {
        let chunk = |kind: &str, text: &str| json!({"sessionId":"one","update":{"sessionUpdate":kind,"content":{"type":"text","text":text}}});
        let updates = vec![
            chunk("agent_message_chunk", "Check"),
            chunk("agent_message_chunk", "ing."),
            chunk("tool_call", ""),
            chunk("tool_call_update", ""),
            chunk("agent_message_chunk", "| Source |"),
            chunk("agent_message_chunk", " Result |\n|---|---|"),
            json!({"sessionId":"other","update":{"sessionUpdate":"agent_message_chunk","content":{"text":"foreign"}}}),
        ];
        assert_eq!(
            reply_text(&updates, "one"),
            "Checking.\n\n| Source | Result |\n|---|---|"
        );
    }
}
