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
    auth_methods: Value,
    auth_attempted: bool,
    read_buffer: Vec<u8>,
    steering: Value,
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
            auth_methods: json!([]),
            auth_attempted: false,
            read_buffer: Vec::new(),
            steering: Value::Null,
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
        loop {
            let b = self.output.fill_buf().await?;
            ensure!(!b.is_empty(), "ACP endpoint disconnected");
            let n = b
                .iter()
                .position(|x| *x == b'\n')
                .map(|n| n + 1)
                .unwrap_or(b.len());
            ensure!(
                self.read_buffer.len() + n <= 24 * 1024 * 1024,
                "ACP message too large"
            );
            self.read_buffer.extend_from_slice(&b[..n]);
            self.output.consume(n);
            if self.read_buffer.last() == Some(&b'\n') {
                break;
            }
        }
        let v: Value = serde_json::from_slice(&std::mem::take(&mut self.read_buffer))?;
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
        let mut forms = crate::elicitation::Callbacks::default();
        let mut permissions = tokio::task::JoinSet::new();
        let mut known_tools = std::collections::HashSet::new();
        let signal = interaction
            .filter(|ctx| ctx.input_id.is_some())
            .map(|ctx| crate::intake::signal(ctx.state, ctx.room));
        let can_steer = method == "session/prompt"
            && self.steering["supported"] == true
            && self.steering["idleBehavior"]
                .as_array()
                .is_some_and(|xs| xs.iter().any(|v| v == "promptRequired"));
        let mut steering_open = can_steer;
        let mut pending: Option<(u64, String, tokio::time::Instant)> = None;
        let mut terminal: Option<Value> = None;
        loop {
            if pending.is_none()
                && let Some(v) = terminal.take()
            {
                if let Some(error) = v.get("error") {
                    if error["code"] == -32002
                        && matches!(method, "session/close" | "session/delete")
                    {
                        return Ok(json!({}));
                    }
                    if error["code"] == -32010 {
                        if let Some(ctx) = interaction
                            && let Some(input) = ctx.input_id
                        {
                            crate::intake::mark(ctx.state, ctx.room, input, "notAccepted")?;
                        }
                        anyhow::bail!("Input recovery required");
                    }
                    return Err(crate::acp_auth::rejected(error, method));
                }
                return Ok(v["result"].clone());
            }
            if steering_open
                && pending.is_none()
                && terminal.is_none()
                && let Some(ctx) = interaction
                && let Some(root) = ctx.input_id
                && let Some(input) = crate::intake::claim_update(ctx.state, ctx.room, root)?
            {
                let input_id = input["request_id"]
                    .as_str()
                    .context("Missing input identity")?
                    .to_owned();
                self.next += 1;
                let request_id = self.next;
                pending = Some((
                    request_id,
                    input_id.clone(),
                    tokio::time::Instant::now() + Duration::from_secs(30),
                ));
                self.send(json!({"jsonrpc":"2.0","id":request_id,"method":"_session/steering","params":{"sessionId":ctx.sid,"prompt":[{"type":"text","text":input["text"],"_meta":{"io.github.bkrabach.agent-observatory":{"inputId":input_id,"author":input["actor"],"sequence":input["sequence"]}}}],"_meta":{"steering":{"inputId":input_id,"idleBehavior":"promptRequired"}}}})).await?;
            }
            let v = tokio::select! {
                ready=permissions.join_next(), if !permissions.is_empty()=>{
                    let (callback,result)=ready.context("Missing permission result")??;
                    self.send(json!({"jsonrpc":"2.0","id":callback,"result":result})).await?;
                    continue;
                }
                ready=forms.completed(), if !forms.is_empty()=>{
                    let (callback, result)=ready?;
                    self.send(json!({"jsonrpc":"2.0","id":callback,"result":result})).await?;
                    continue;
                }
                _=input_signal(&signal), if method=="session/prompt" && steering_open=>{continue;}
                _=steering_deadline(pending.as_ref().map(|p|p.2)), if pending.is_some()=>{
                    if let Some((_,input,_))=pending.take() && let Some(ctx)=interaction {crate::intake::mark(ctx.state,ctx.room,&input,"unknown")?;}
                    steering_open=false;
                    continue;
                }
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
            if v.get("method").is_none()
                && pending
                    .as_ref()
                    .is_some_and(|(request, _, _)| v["id"] == *request)
            {
                let (_, input, _) = pending.take().unwrap();
                if let Some(ctx) = interaction {
                    let status = match v["result"]["outcome"].as_str() {
                        Some("injected") => "received_remote",
                        Some("promptRequired") => {
                            steering_open = false;
                            "promptRequired"
                        }
                        _ => {
                            steering_open = false;
                            "unknown"
                        }
                    };
                    crate::intake::mutate(ctx.state, ctx.room, |s| {
                        if let Some(m) = s["messages"].as_array_mut().and_then(|ms| {
                            ms.iter_mut()
                                .find(|m| m["request_id"] == input && m["role"] == "user")
                        }) && (m["delivery"] == "submitted" || status == "unknown")
                        {
                            m["delivery"] = json!(status);
                        }
                    })?;
                }
                continue;
            }
            if v.get("method").is_none() && v["id"] == id {
                if let Some(ctx) = interaction
                    && let Some(root) = ctx.input_id
                    && let Some(receipts) =
                        v["result"]["_meta"][crate::intake::META]["inputReceipts"].as_array()
                {
                    for receipt in receipts {
                        crate::intake::receipt(ctx.state, ctx.room, root, receipt)?;
                    }
                }
                terminal = Some(v);
                continue;
            }
            if v.get("id").is_some() && v.get("method").is_some() {
                if v["method"] == "elicitation/create" {
                    let result = (|| -> Result<_> {
                        let ctx = interaction.context("No initiating person")?;
                        let p = &v["params"];
                        let session_scope = !ctx.sid.is_empty()
                            && p["sessionId"] == ctx.sid
                            && p.get("requestId").is_none();
                        let request_scope = p["requestId"] == id
                            && p.get("sessionId").is_none()
                            && p.get("toolCallId").is_none();
                        ensure!(session_scope || request_scope, "Invalid elicitation scope");
                        if let Some(tool) = p.get("toolCallId") {
                            ensure!(
                                tool.as_str().is_some_and(|t| known_tools.contains(t)),
                                "Unknown tool scope"
                            );
                        }
                        ctx.state
                            .elicitations
                            .request(
                                ctx.room,
                                ctx.actor,
                                "Agent",
                                p,
                                Duration::from_secs(300).min(
                                    ctx.deadline
                                        .saturating_duration_since(tokio::time::Instant::now()),
                                ),
                            )
                            .map_err(anyhow::Error::msg)
                    })();
                    match result.and_then(|request| forms.start(v["id"].clone(),request)) {
                        Ok(()) => (),
                        Err(_) => self.send(json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32602,"message":"Unsupported or incorrectly scoped interaction"}})).await?,
                    }
                } else if v["method"] == "session/request_permission" {
                    if let Some(ctx) = interaction.filter(|c| v["params"]["sessionId"] == c.sid) {
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
                                let callback=v["id"].clone();
                                permissions.spawn(async move {(callback,request.wait().await)});
                            }
                            Err(_) => self.send(json!({"jsonrpc":"2.0","id":v["id"],"result":crate::permissions::cancelled()})).await?,
                        }
                    } else {
                        self.send(json!({"jsonrpc":"2.0","id":v["id"],"result":crate::permissions::cancelled()})).await?;
                    }
                } else {
                    self.send(json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32601,"message":"This observatory client does not offer that capability"}})).await?;
                }
            } else if v["method"] == "$/cancel_request" {
                forms.cancel(&v["params"]["requestId"]);
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
                    if let Some(root) = ctx.input_id {
                        crate::intake::receipt(
                            ctx.state,
                            ctx.room,
                            root,
                            &v["params"]["update"]["_meta"][crate::intake::META]["inputReceipt"],
                        )?;
                    }
                    if let Some(tool) = v["params"]["update"]["toolCallId"].as_str()
                        && v["params"]["update"]["sessionUpdate"] == "tool_call"
                    {
                        known_tools.insert(tool.to_owned());
                    }
                    progress(ctx.state, ctx.room, &v["params"]["update"])?;
                }
                updates.push(v["params"].clone());
            }
        }
    }
    async fn session_request(
        &mut self,
        method: &str,
        params: Value,
        updates: &mut Vec<Value>,
        ctx: &Interaction<'_>,
    ) -> Result<Value> {
        let first = self
            .request(method, params.clone(), updates, Some(ctx))
            .await;
        if !first
            .as_ref()
            .err()
            .is_some_and(crate::acp_auth::authentication_required)
            || self.auth_attempted
        {
            return first;
        }
        self.auth_attempted = true;
        let choice = crate::acp_auth::choice(&self.auth_methods)
            .map_err(|_| crate::acp_auth::AuthenticationRequired)?;
        let request = ctx
            .state
            .elicitations
            .request(
                ctx.room,
                ctx.actor,
                "Agent",
                &choice,
                Duration::from_secs(300).min(
                    ctx.deadline
                        .saturating_duration_since(tokio::time::Instant::now()),
                ),
            )
            .map_err(anyhow::Error::msg)?;
        let mut cancellation = ctx
            .state
            .cancellations
            .lock()
            .unwrap()
            .get(ctx.room)
            .map(|(_, tx)| tx.subscribe());
        let answer = tokio::select! {v=request.wait()=>v,_=cancel_signal(&mut cancellation)=>crate::elicitation::cancelled(),_=self.child.wait()=>anyhow::bail!("Agent disconnected during sign-in")};
        ensure!(answer["action"] == "accept", "Agent sign-in cancelled");
        self.request(
            "authenticate",
            json!({"methodId":answer["content"]["method_id"]}),
            updates,
            Some(ctx),
        )
        .await?;
        self.request(method, params, updates, Some(ctx)).await
    }
    async fn initialize(&mut self, ctx: Option<&Interaction<'_>>) -> Result<Value> {
        let init=self.request("initialize",json!({"protocolVersion":1,"clientInfo":{"name":"agent-observatory","version":env!("CARGO_PKG_VERSION")},"clientCapabilities":if ctx.is_some(){json!({"elicitation":{"form":{},"url":{}}})}else{json!({})}}),&mut vec![], ctx).await?;
        ensure!(init["protocolVersion"] == 1, "Unsupported ACP version");
        self.auth_methods = init["authMethods"].clone();
        self.steering = init["_meta"]["steering"].clone();
        let mut init = init;
        init["_meta"]["io.github.bkrabach.agent-observatory"]["authentication"] =
            crate::acp_auth::authentication(&init);
        Ok(init)
    }
}
pub async fn probe(state: &AppState, config: &Value) -> ApiResult<Value> {
    let work = async {
        let mut client = Client::start(state, config).await?;
        client.initialize(None).await
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
/// Operator-only discovery. No transcript or credentials are copied between
/// clients; this lists only the configured working directory.
pub async fn catalog(
    state: &AppState,
    config: &Value,
    cursor: Option<&str>,
    delete: Option<&str>,
) -> ApiResult<Value> {
    let work = async {
        let mut client = Client::start(state, config).await?;
        let init = client.initialize(None).await?;
        let capability = if delete.is_some() { "delete" } else { "list" };
        ensure!(
            init["agentCapabilities"]["sessionCapabilities"][capability].is_object(),
            "Agent does not advertise this session operation"
        );
        let mut params = json!({"cwd":config.get("session_cwd").unwrap_or(&config["cwd"])});
        if let Some(cursor) = cursor {
            ensure!(cursor.len() <= 8192, "Invalid session cursor");
            params["cursor"] = json!(cursor);
        }
        let result = if let Some(sid) = delete {
            ensure!(!sid.is_empty() && sid.len() <= 1024, "Invalid session id");
            client
                .request(
                    "session/delete",
                    json!({"sessionId":sid}),
                    &mut vec![],
                    None,
                )
                .await?
        } else {
            client
                .request("session/list", params.clone(), &mut vec![], None)
                .await?
        };
        if delete.is_some() {
            return Ok::<_, anyhow::Error>(json!({"removed_from_agent_history":true}));
        }
        let entries = result["sessions"]
            .as_array()
            .context("Invalid session catalog")?;
        ensure!(entries.len() <= 1000, "Agent session page exceeds limit");
        let entries:Vec<Value>=entries.iter().filter(|v|v["cwd"]==params["cwd"]).filter_map(|v|{
            let sid=v["sessionId"].as_str().filter(|s|!s.is_empty()&&s.len()<=1024)?;
            Some(json!({"sessionId":sid,"title":v["title"].as_str().unwrap_or("Untitled session").chars().take(200).collect::<String>(),"updatedAt":v["updatedAt"].as_str().unwrap_or("").chars().take(100).collect::<String>()}))
        }).collect();
        let next = result["nextCursor"].as_str().filter(|s| s.len() <= 8192);
        Ok(
            json!({"sessions":entries,"nextCursor":next,"can_delete":init["agentCapabilities"]["sessionCapabilities"]["delete"].is_object()}),
        )
    };
    tokio::time::timeout(Duration::from_secs(30),work).await.map_err(|_|ApiError::bad("Session operation timed out; inspect the agent before retrying"))?.map_err(|e|ApiError::bad(if crate::acp_auth::authentication_required(&e){"Agent sign-in required; use Talk to complete sign-in first"}else{"Agent session operation unavailable; check its advertised capabilities and connection"}))
}
pub async fn disconnect(state: &AppState, context: &str) -> ApiResult<()> {
    let live = state.acp_clients.lock().unwrap().remove(context);
    if let Some(live) = live {
        let mut live = live.lock().await;
        if live.init["agentCapabilities"]["sessionCapabilities"]["close"].is_object() {
            let sid = live.sid.clone();
            tokio::time::timeout(
                Duration::from_secs(10),
                live.client
                    .request("session/close", json!({"sessionId":sid}), &mut vec![], None),
            )
            .await
            .map_err(|_| ApiError::bad("Disconnect timed out; the local connection was released"))?
            .map_err(|_| {
                ApiError::bad(
                    "Agent did not confirm closing the session; the local connection was released",
                )
            })?;
        }
    }
    Ok(())
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
    prompt_input(state, context, config, prior, text, actor, None).await
}
pub async fn prompt_input(
    state: &AppState,
    context: &str,
    config: &Value,
    prior: Option<&str>,
    text: &str,
    actor: &str,
    input_id: Option<&str>,
) -> ApiResult<Value> {
    run_input(state, context, config, prior, Some(text), actor, input_id).await
}
pub async fn recover_inputs(
    state: &AppState,
    context: &str,
    config: &Value,
    prior: Option<&str>,
    actor: &str,
) -> ApiResult<Value> {
    run_input(state, context, config, prior, None, actor, None).await
}
async fn run_input(
    state: &AppState,
    context: &str,
    config: &Value,
    prior: Option<&str>,
    text: Option<&str>,
    actor: &str,
    input_id: Option<&str>,
) -> ApiResult<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    let work = async {
        let cached = state.acp_clients.lock().unwrap().get(context).cloned();
        ensure!(
            text.is_some() || prior.is_some() || cached.is_some(),
            "No saved session to recover"
        );
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
            let setup = Interaction {
                state,
                room: context,
                actor,
                sid: prior.unwrap_or(""),
                deadline,
                input_id: None,
            };
            let init = tokio::select! {
                _=cancel_signal(&mut setup_cancel)=>anyhow::bail!("Turn cancelled"),
                result=client.initialize(Some(&setup))=>result?,
            };
            let mut updates = vec![];
            let sid = if let Some(prior) = prior {
                let restore = if init["agentCapabilities"]["sessionCapabilities"]["resume"]
                    .is_object()
                {
                    "session/resume"
                } else if init["agentCapabilities"]["loadSession"] == true {
                    "session/load"
                } else {
                    anyhow::bail!(
                        "This endpoint cannot restore a disconnected session. Its transcript is saved; start a new conversation."
                    )
                };
                tokio::select! {
                  _=cancel_signal(&mut setup_cancel)=>anyhow::bail!("Turn cancelled"),
                  result=client.session_request(
                        restore,
                        json!({"sessionId":prior,"cwd":config.get("session_cwd").unwrap_or(&config["cwd"]),"mcpServers":[]}),
                        &mut updates,
                        &setup,
                    )=>result?,
                };
                resumed = true;
                prior.to_owned()
            } else {
                let created = tokio::select! {
                  _=cancel_signal(&mut setup_cancel)=>anyhow::bail!("Turn cancelled"),
                  result=client.session_request(
                        "session/new",
                        json!({"cwd":config.get("session_cwd").unwrap_or(&config["cwd"]),"mcpServers":[]}),
                        &mut updates,
                        &setup,
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
        if input_id.is_some() || text.is_none() {
            // Save the binding before any prompt bytes are sent, including failure paths.
            crate::intake::mutate(state, context, |s| {
                s["acp_session_id"] = json!(sid);
                s["steering"] = json!(live.init["_meta"]["steering"]["supported"] == true);
            })?;
            if state.read("sessions", context)?["recovery_requested"] == true {
                let record = state.read("sessions", context)?;
                if live.init["_meta"]["steering"]["receiptsMethod"] == "_session/input_receipts"
                    && live.init["_meta"]["steering"]["acknowledgeMethod"]
                        == "_session/acknowledge_inputs"
                {
                    let remote = live
                        .client
                        .request(
                            "_session/input_receipts",
                            json!({"sessionId":sid}),
                            &mut vec![],
                            None,
                        )
                        .await?;
                    let remote_receipts = remote["inputReceipts"]
                        .as_array()
                        .context("Invalid input receipts response")?;
                    let ids: Vec<_> = remote_receipts
                        .iter()
                        .filter_map(|r| {
                            let id = r["inputId"].as_str()?;
                            let local = record["messages"]
                                .as_array()?
                                .iter()
                                .find(|m| m["request_id"] == id && m["role"] == "user")?;
                            (local["delivery"] == "unknown"
                                || r["state"] == "unknown"
                                || (r["state"] == "incorporated" && r["recorded"] == false))
                                .then_some(id)
                        })
                        .collect();
                    if !ids.is_empty() {
                        let acknowledged=live.client.request("_session/acknowledge_inputs",json!({"sessionId":sid,"inputIds":ids,"disposition":"proceed_without_replay"}),&mut vec![],None).await?;
                        let receipts = acknowledged["inputReceipts"]
                            .as_array()
                            .context("Invalid acknowledgment response")?;
                        ensure!(
                            ids.iter().all(|id| receipts
                                .iter()
                                .any(|r| r["inputId"] == *id && r["reconciled"] == true)),
                            "Input recovery was not confirmed"
                        );
                    }
                }
                crate::intake::mutate(state, context, |s| {
                    for m in s["messages"].as_array_mut().unwrap() {
                        if m["delivery"] == "unknown" {
                            m["acknowledged"] = json!(true);
                        }
                    }
                    s["recovery_requested"] = json!(false);
                })?;
            }
        }
        if text.is_none() {
            return Ok(json!({"session_id":sid,"recovered":true}));
        }
        if let Some(input_id) = input_id {
            if !crate::intake::authorized(state, context, input_id) {
                crate::intake::mark(state, context, input_id, "notAccepted")?;
                anyhow::bail!("Input authorization changed");
            }
            crate::intake::mark(state, context, input_id, "submitted")?;
        }
        let mut updates = vec![];
        let result = live
            .client
            .request(
                "session/prompt",
                json!({"sessionId":sid,"prompt":[{"type":"text","text":text.unwrap()}]}),
                &mut updates,
                Some(&Interaction {
                    state,
                    room: context,
                    actor,
                    sid: &sid,
                    deadline,
                    input_id,
                }),
            )
            .await?;
        live.used_at = crate::state::now_ms();
        let reply = reply_text(&updates, &sid);
        Ok::<_, anyhow::Error>(
            json!({"session_id":sid,"resumed":resumed,"resumable":live.init["agentCapabilities"]["loadSession"]==true || live.init["agentCapabilities"]["sessionCapabilities"]["resume"].is_object(),"reply":reply,"media":reply_media(&updates,&sid),"stop_reason":result["stopReason"],"capabilities":live.init["agentCapabilities"],"updates":updates}),
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
        Ok(Err(e)) if e.to_string() == "Input authorization changed" => Err(ApiError::bad(
            "This message's sign-in or access expired before it could be sent. Sign in and send a new message.",
        )),
        Ok(Err(e)) if e.to_string() == "Input recovery required" => Err(ApiError::bad(
            "The agent needs review of earlier input delivery before continuing. Inspect its session and effects before authorizing recovery.",
        )),
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

async fn input_signal(signal: &Option<std::sync::Arc<tokio::sync::Notify>>) {
    if let Some(signal) = signal {
        signal.notified().await;
    } else {
        std::future::pending::<()>().await;
    }
}
async fn steering_deadline(deadline: Option<tokio::time::Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
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
    input_id: Option<&'a str>,
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
