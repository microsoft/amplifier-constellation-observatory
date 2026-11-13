# Observatory API reference

Exact routes, scopes, data and callback behavior for the current service.
The [local agreement](contracts.md) explains the promises these interfaces serve.

All routes below are relative to `/api/v1`. Browser sessions use the installation's configured authentication and same-origin CSRF protection. HTTPS sessions use Secure cookies. Explicit desktop-local mode uses a local password and a loopback-only HTTP listener, with a non-Secure cookie restricted to that local origin. Existing Linux PAM/HTTPS installations retain their configuration. API credentials are bearer tokens with explicit scopes; `admin` includes all scopes. TLS validation remains enabled for remote providers, with an optional locally supplied CA file. Redirects are not followed with remote credentials.

The browser opens the systems map at `/`. Detailed administration screens remain
at `/advanced`; both use this API and the same authentication.

### Native local launch

Opening the native application without a subcommand creates or opens its own
installation. First setup requires a short-lived capability delivered in the
local browser URL fragment; the fragment is removed from browser history and is
not sent as an HTTP query. Setup publishes a new private directory atomically and
refuses an existing destination. The setup password is stored only as an Argon2id
hash. Sign-in startup is an optional per-user OS task; failure to register it is
reported without discarding the installation.

Local HTTP is explicit in config (`auth_mode: local-password`,
`web_transport: local-http`) and accepts only loopback binding, a matching local
origin and same-origin browser requests. This HTTP listener cannot be used for
remote pairing or embedding. An explicitly enabled, separate helper-only HTTPS
listener is described under [computer pairing](#computer-pairing-and-presence).
The local CLI proves the installation identity before transmitting
its management credential, using a nonce-bound HMAC response and the same TCP
connection for the subsequent request. HTTPS installations retain normal TLS
validation. This local proof does not authorize remote clients.

| Scope | Operations |
| --- | --- |
| `observe` | descriptor, status, endpoint/host/place inventory, inspections, relationships, experiment reports, activity |
| `work` | read responsibility and organization data through optional providers |
| `work:execute` | explicitly authorize background work and change scheduler settings through optional providers |
| `memory:read` | read attributed memory through optional providers |
| `memory` | read and write attributed memory through optional providers |
| `manage` | connect/edit/detach endpoints, edit place groupings, store credentials, invoke configuration and connection commands, work-record writes, submit experiment reports |
| `interact` | ACP negotiation, sessions, prompts, disconnect |
| `provision` | pair/revoke computers, authorize/edit hosts, plan/install, job records and output |
| `ui` | read and control browser view state |
| `admin` | create/revoke scoped tokens and all operations |

An observation credential does not permit agent prompts, configuration, private work details, installation execution, or reading installation logs. The remote endpoint's credential imposes its own independent limits. An inventory relationship does not grant access or configure a live connection.

## Endpoint registry

`GET/POST endpoints`, `GET/PUT/DELETE endpoints/{id}`. PUT and DELETE require `expected_revision`. POST accepts:

```json
{
  "label": "Release agent",
  "kind": "agent",
  "ui_url": "https://agent.example:18443",
  "acp": {
    "transport": "stdio",
    "command": "/path/to/attach",
    "args": [],
    "cwd": "/path/to/workspace",
    "env": {},
    "secret_env": {}
  },
  "management": {
    "provider": "agent-instance",
    "base_url": "https://agent.example:18443",
    "credential_ref": "release-management",
    "access": "manage",
    "expected_id": "independently-verified-installation-id"
  }
}
```

ACP, management and UI are individually optional, but at least one is required. `POST endpoints/{id}/inspect` records a dated management observation. `POST endpoints/{id}/probe` negotiates standard ACP. Neither test implies successful work or persona fidelity. The UI pins a newly inspected installation ID when connecting; API callers should inspect and pin deliberately.

Optional `host_id` is a saved host UUID or null. Omitting it on PUT preserves the
association; null clears it. A host cannot be removed while endpoints refer to it.
Unassigned endpoints remain visible. This is an explicit inventory association,
not a reachability check or an installation move. Inspection may return HTTP 200
with `health: "unavailable"`; check the observation before claiming connection.

Agent entries use `kind: "agent"` even when they offer ACP alone. Register their
conversation adapters separately with `kind: "adapter"` and a `uses` relationship
from adapter to agent. An adapter entry does not automatically register its agent;
its management credentials do not confer access to the agent's ACP endpoint.

Both inventory GET routes return `observation` for management and
`acp_observation` for ACP. ACP observations persist successful handshakes or failed
checks, their timestamp, negotiated implementation/capabilities, and the checked
endpoint revision. After an endpoint edit, a prior ACP observation is omitted
until that revision is checked. A failed ACP check still returns an HTTP error
and leaves a durable failure observation. Neither check replaces the other's
data. The dashboard checks every attached interface, includes ACP-only agents,
and continues checking the other endpoints when one fails. Running an ACP check
still requires `interact`; reading its recorded result requires `observe`.

`POST endpoints/{id}/operations/{operation}` sends a body to an explicit management mapping. The provider's registry chooses the HTTP method/path and required Observatory scope; callers cannot select an arbitrary URL. Built-in operations include `config`, `validate-config`, `apply-config`, `lifecycle`, `work`, `organization`, `profile-use`, `sessions`, `activity` and `permissions` where the particular provider implements them. Unsupported remote features return an explicit failure; they do not fall back to private files.

For `generic-json`, `management.operations` contains `{id,method,path,access}`. Paths are bounded relative API paths; observe and work operations must be GET; descriptor/status always require observe access and GET. Any write also requires a management connection explicitly set to `manage`. The operator can map a different implementation's explicit interfaces while leaving ACP unchanged.

### Saved service connections

Agent Instance mappings require the pinned identity, advertised capability
`constellation.service-connections/1`, and the exact advertised operation method
and path. `POST endpoints/{id}/operations/service-connections {}` reads the
installation's catalog with `observe`. The `service-connection-action` operation
requires `manage` and forwards one of these bodies unchanged:

```json
{"connection_id":"slack-design","revision":"catalog-revision","action":"disconnect","request_id":"49c18585-903d-4481-bd5f-7b5863aa7b1a"}
```

Use `action: "connect"` for a saved connection. An advertised typed Add accepts
`{service_id,revision,action:"add",request_id,values}`. IDs are bounded, request IDs
must be canonical UUIDs, and values are bounded strings, integers or booleans;
credential fields are rejected. Callers cannot select a host command or provider
destination through this mapping. Available choices, revision checks, durable request reconciliation and
actual/desired state remain owned by the
[installation's connection API](https://github.com/microsoft/amplifier-constellation-instance/blob/main/docs/service-connections.md).
An accepted request is not proof of completed connection. Read the catalog to
observe settlement; an unavailable catalog is unknown, not disconnected.

### Display places

`GET/POST places`, `PUT/DELETE places/{id}`. Reading requires `observe`; changes
require `manage`, with `expected_revision` on PUT/DELETE. The server supplies each
record's UUID and revision. A place groups connections across installations:

```json
{
  "label":"Studio",
  "service":"slack",
  "kind":"workspace",
  "external_id":"T1234",
  "rooms":[{"id":"C1234","label":"#design"}],
  "bindings":[{"endpoint_id":"9c7b4074-0209-4d7d-9d9a-a7a0919689ab","connection_id":"slack-design","room_id":"C1234"}]
}
```

Kinds are `workspace`, `server` or `experience`; service is a lowercase slug.
`external_id` and binding `room_id` are optional. Rooms and bindings may be empty.
Each binding must name an existing endpoint and, when supplied, a declared room.
The same endpoint/connection pair can belong to only one place. This registry
never creates a remote room, changes a connector or certifies live presence.
Unmapped connections remain separate by endpoint and connection ID; matching
labels do not merge them. Removed endpoints or missing catalog entries leave
unknown bindings rather than invented status.

Labels are at most 160 bytes; opaque external, room and connection IDs are at
most 200 ASCII letters, digits or `._:-`. A place permits up to 500 rooms and
1,000 bindings. There are no URL, credential or command fields; unknown fields
are rejected.

### Optional memory, continued work and engine settings

The reference client summarizes acceptance criteria, delivered versions and named
reviewers from the agent's optional work contract. **Inspect work and review**
opens the exact responsibility in that agent's own dashboard. The embedding
command and selected work are programmatic view state; authentication and
approval remain with that agent.

Agent Instance's additional operations are offered only when its current descriptor advertises their exact path and method. A legacy descriptor without an operation list does not imply support. Calls recheck the pinned remote identity and advertised operation before using the configured credential. The Observatory never substitutes a private file or database read.

Mappings include `memory`, `remember`, `memory-item`, `memory-command`, `scheduler`, `configure-scheduler`, `work-item`, `work-command`, `work-execution`, `execute-work`, `native-config`, `native-schema`, `validate-native-config`, and `apply-native-config`. Operations for a particular memory/work record take `{resource_id:<UUID>,payload?:<remote request>}`; other operations accept the remote request directly. The UUID is substituted into a fixed provider path and cannot select an arbitrary URL.

For example, the execution body is forwarded without changing its explicit revision, reason, request ID or budget:

```json
{
  "resource_id": "13408493-34ab-4ee4-b723-3edd8b3e3cf6",
  "payload": {
    "type": "resume",
    "run_id": "a3c04fb0-c176-4399-97ac-7f9a2d9e8586",
    "expected_revision": 3,
    "expected_work_revision": 4,
    "request_id": "deliberate-continuation",
    "reason": "Continue from the inspected planning checkpoint.",
    "budget": {"max_turns":4,"max_seconds":300,"max_output_bytes":131072,"max_tool_calls":30}
  }
}
```

Send this to `POST endpoints/{id}/operations/execute-work`. `work` and `manage` grants do not authorize execution. Memory grants are also separate from observation and work. The target service independently enforces the credential's scopes and its saved authorization policy. Native configuration mappings require management access; schema and configuration formats belong to the target engine, not the fleet.

The Memory tab shows attributed alternatives, audience, source counts and source details. The Background work tab shows real run states, cumulative turns/tool calls, budgets and reports awaiting review. Both tabs require advertised operations. Their edit buttons open the independent agent's own UI; model/native capability settings use that same embedded service UI. The parent sends only the published `agent-instance.ui/1` navigation command to the exact configured origin. The child must separately allow that origin and authenticate the browser. A visible retry button accommodates delayed sign-in. This sends no model prompt and grants no domain authority.

These reference-client views are programmable: use `tab: "memory"` or `"background"`; `draft.memory_details` and `draft.run_report` select source/report disclosures. Use `tab: "embedded"` and `draft.embedded_route: "memory"`, `"work"`, or `"configuration"` to display the corresponding independent screen. All domain changes remain available through the operation mappings above, without driving browser pixels.

`GET credentials` returns names only. `POST credentials {name,value}` writes a private credential. Add `create_only:true` to refuse an existing name atomically; the new-agent connection form uses this mode so it cannot replace another installation’s grant. Environment secret references name those files. The host control credential cannot be referenced by an attached endpoint.

## Conversations

`GET/POST sessions`, `GET sessions/{id}`, `POST sessions/{id}/prompt`, `POST sessions/{id}/close`.

Create with `{endpoint_id,title?}`. Prompt with `{text,request_id:<UUID>}`. Duplicate identical requests return the saved record; a reused request ID with different text is rejected. A prompt is asynchronous and persists its running/result state before the UI disconnects. Poll the session for completion. No automatic retries occur after an ambiguous failure.

Send remains available during work. Every accepted message has a stable
`request_id`, author, increasing `sequence`, and durable `delivery` state. Retries
with the same ID, text and actor return the existing message; changing the text
or actor is rejected. Each conversation has one worker and at most one normal
ACP prompt in flight. Up to 32 pending inputs and 1 MiB of pending text are
accepted; excess messages are rejected before recording acceptance. Individual
messages remain limited to 100,000 UTF-8 bytes.

An endpoint advertising `_meta.steering.supported` and `promptRequired` idle
behavior receives `_session/steering` with the stable input ID, original author,
sequence, and existing ACP session. Only one steering request is unresolved at a
time. Native receipt updates/final receipts are applied only to the matching
conversation and original turn. Generic ACP uses a FIFO next-prompt queue.
`promptRequired` is the only automatic steering-to-prompt fallback; failed,
malformed, timed-out, unexpected `startedNewTurn`, and lost outcomes become
uncertain and are never blindly replayed. The original prompt can complete while
an outstanding steering response settles.

| Delivery value | Meaning |
| --- | --- |
| `received` | Saved locally, waiting to send |
| `submitted` | Sending; acceptance is not yet known |
| `received_remote`, `remoteQueued` | Agent acknowledged input or model queued it; incorporation is not claimed |
| `incorporated` | An explicit runtime receipt confirms inclusion in model context |
| `completed` | A normal prompt completed, displayed as “Sent” |
| `delivered` | A generic steering acknowledgment and normal turn completion, without a model-incorporation claim |
| `promptRequired` | Definite unapplied input waiting for an ordinary next prompt |
| `unknown` | Delivery needs review; do not replay |
| `notAccepted`, `cancelled` | Not admitted or explicitly stopped; do not retry automatically |

On process restart, unfinished submitted inputs become `unknown`; unsent inputs
keep their order. `POST sessions/{id}/inputs/resume` with
`{acknowledge_unknown:true}` is an explicit decision to continue **without
replaying uncertain inputs**, after inspecting actual work. If the endpoint
advertises `_session/input_receipts` and `_session/acknowledge_inputs`, this
loads the existing session, reads its receipts and acknowledges the matching
uncertain inputs with `proceed_without_replay`. It performs recovery even with
no new queued message and does not issue a model prompt for that recovery.
Unknown transcript entries retain their truthful state plus `acknowledged:true`.
Without those optional methods, the local acknowledgment permits only the
saved, definitely unsent queue to continue. Unsupported restoration still fails
visibly; it never creates a replacement session and replays history.

Use `{acknowledge_unknown:false}` to resume an interrupted but definitely unsent
queue. This route requires `interact` and ordinary browser CSRF protection.
Queued inputs retain their original credential reference privately. Expiry,
revocation or scope removal before dispatch changes their state to `notAccepted`;
recovery does not renew authority. Endpoint revision changes also block queued
work. Ordinary follow-up messages never cancel the current prompt or its pending
approval; explicit Stop cancels the active turn and pending local inputs.

All controls are API-accessible. The browser saves unconfirmed sends with their original request ID in the
programmable view field `draft.pending_send` (and a session-storage fallback), preserves the composer draft across
polling/refresh, and offers exact-message retry after a lost HTTP response.
Receipt labels and recovery controls render from the saved session record.

Each conversation retains a separate live ACP process. Inactive processes expire after 15 minutes; at most 32 are held. An endpoint advertising `sessionCapabilities.resume` can restore without replay; otherwise `loadSession` restores with replay. Other endpoints can carry on through their existing connection; after it closes, their saved transcript remains readable and a new conversation is required. The Observatory does not claim to restore a session by silently replaying text into a new one.

The client supports text, live tool/plan progress, explicit permission decisions, form/URL elicitation, agent-method authentication, and saved session loading. It advertises no client filesystem/terminal callbacks; native agent-side tools remain governed by that endpoint. Prompts time out after ten minutes. Stop and timeout send standard ACP `session/cancel`, allow a short completion window, then close the subprocess; work already performed is not undone. Closing an idle connection preserves the transcript.

`GET /api/v1/sessions/{id}` includes bounded `progress` (text, tool status, plan, revision), the initiating actor's pending `permissions`, and `can_cancel`. Private reasoning chunks and updates for another ACP session are not exposed. The UI polls progress without replacing the message draft. Completed turn progress is retained with the session; approval arguments remain in memory only.

`POST /api/v1/sessions/{id}/permissions/{request_id}` with `{"option_id":"offered-option-id"}` returns that exact agent-offered choice. Use `{"option_id":null}` to cancel the request. Requests expire within three minutes, are bound to this conversation and the user/credential that started the turn, and are single use. Other actors cannot see or answer them. Expiry, disconnection, cancellation and restart never grant permission. Audit entries omit tool arguments and option text. An `allow_always` choice is explicitly labelled in the UI and delegates that remembered policy to the agent; the client does not silently choose it.

`POST /api/v1/sessions/{id}/cancel` stops a turn started by the same actor. Both decision and cancellation require `interact` and the normal CSRF protection for browser sessions. API automation uses its own credential identity; a different credential does not inherit a human's pending decisions. View commands can set `permission_details` to a pending request UUID (or null) to show/hide action details, and `focus` can target `permission-<UUID>-details`, `permission-<UUID>-option-<index>`, or `permission-<UUID>-cancel` to focus the displayed controls. Generic ACP negotiation is unchanged.

## Machines and installers

`GET/POST hosts`, `PUT/DELETE hosts/{id}`. Editing/deleting requires `expected_revision`.

```json
{
  "label": "agent-host",
  "transport": "ssh",
  "address": "agent-host",
  "install_roots": ["/home/operator/.local/share/agents"],
  "recipes": [{
    "id": "agent-instance",
    "version": "specific-release-id",
    "argv": ["/path/to/pinned/installer", "--home", "{home}", "--name", "{name}", "--port", "{port}"],
    "timeout_seconds": 1800
  }]
}
```

The host/recipe is a privileged operator configuration. Pin actual executables or release directories; a version label alone does not verify a mutable executable. `local` uses an argument array directly. `ssh` runs a fixed Python dispatcher with JSON on stdin; it does not evaluate a model-supplied shell command. SSH requires a working configured key and known host, plus remote Python. The initial SSH dispatcher targets POSIX hosts; native Windows installers can be invoked through another explicit host recipe or their own management wrapper.

Empty `recipes` and `install_roots` together save inventory only. Saving a host
does not contact it or install a pairing helper. Installation requires an approved
recipe and an allowed root. Paired computers use the separate interface below;
pairing does not grant an installer or remote shell.

### Computer pairing and presence

The descriptor advertises `capabilities.machine_pairing:true` for this schema-1
interface. Download availability is reported by the pairing endpoints.

Desktop packages retain verified helper archives for Windows, macOS and Linux,
each on x64 and ARM64. An application update stages and verifies those archives
with its application inventory; rollback restores the captured prior helper
tree. Existing manually staged inventories remain valid update baselines.
Server installations may instead stage the published helper bundle using the
[owner command](../README.md#advanced-linux-installation).

`GET machine-network` requires `provision` and reports `mode`, `enabled`,
`available`, `endpoint`, `revision` and local address choices
`addresses:[{address,label}]`. Local mode also reports the saved `address`,
`request_id` and any `error`. `POST machine-network` requires `admin` and takes
`{request_id:<UUID>,expected_revision,enabled,address?:<IP>}`. Enable selects a
current non-loopback interface address; the service chooses and retains its
port. An exact request retry returns its saved result; a changed revision is
`409`. Read status after a lost reply before requesting another change.

In desktop-local mode this opens a separate HTTPS listener exposing only helper
redemption, heartbeat and self-revocation. It exposes no browser, sign-in,
administration or installation routes. Its private CA is retained and its public
certificate accompanies tailored setup downloads. The existing local browser
listener is unchanged. Disabling access closes this helper listener without
revoking computer grants. An unavailable address or port is reported without
preventing local startup. `available` means this listener is ready locally; it
does not prove another computer's route or firewall access. Existing HTTPS mode
reports `mode:"configured_https"` and keeps its configured public URL; this
interface cannot reconfigure it.

`POST machine-pairings {request_id:<UUID>,install_agents?:bool}` creates a ten-minute pairing. An exact
request retry returns the same record. `GET machine-pairings` lists pairings;
`GET machine-pairings/{id}` reads one. Pairings and their codes remain private to
the credential or browser actor that created them. These operations require
`provision`, as do setup downloads and `POST machine-pairings/{id}/cancel {}`. Browser writes retain
the normal CSRF protection. Cancellation closes a waiting pairing, never an
already accepted computer grant.

Create/read responses contain `pairing:{id,code,state,expires_at,host_id}` and
`downloads:[{platform,label,architecture,url,filename}]`. List returns `pairings`
and available `downloads`. The code is eight characters, displayed with a
separator. Five incorrect attempts lock that pairing; expired/cancelled codes
cannot create a grant. Pairing identifiers are independently random UUIDs.

`GET machine-pairings/{id}/setup/{platform}` returns a setup archive from this
installation's verified helper artifacts. Platform IDs are `windows-x86_64`,
`windows-aarch64`, `macos-aarch64`, `macos-x86_64`, `linux-aarch64` and `linux-x86_64`; unavailable
artifacts are not advertised. The archive's `observatory-pairing.json` contains
`schema:1`, HTTPS `observatory_url`, `observatory_id`, `observatory_name`,
`pairing_id`, `expires_at` and optional `ca_pem`. Current installation-capable
artifacts also receive `install_agents`; older artifacts omit it. It contains no
code or credential.
The helper validates TLS, the pinned Observatory identity and its origin, without
following redirects. Downloading setup does not establish network reachability.
Unsigned Mac preview setups also carry this public file inside the app's
`Contents/Resources` so lookup survives app translocation. This is not a signing
solution: changing bundle resources after signing invalidates that signature;
a future signed package needs an appropriate external-configuration flow.

The helper persists a random machine ID, request ID and 256-bit credential before
calling `POST machine-pairings/{id}/redeem`:

```json
{
  "schema": 1,
  "observatory_id": "UUID",
  "code": "ABCD-EFGH",
  "request_id": "persisted UUID",
  "machine_id": "persisted UUID",
  "machine_credential": "64 random hexadecimal characters",
  "name": "My computer",
  "os": "linux",
  "arch": "x86_64",
  "helper_version": "0.1.0"
}
```

Redemption uses that pairing ID and code, without an Observatory admin credential.
The receipt is `{schema:1,pairing_id,observatory_id,host_id,machine_id,
access:["observe"],heartbeat_seconds:15}`. If the invitation requested
`install_agents:true` and the computer owner selected it in the helper, redemption
also sends `install_agents:true` and access includes `"install-agents"`. Missing
fields preserve observation-only behavior. The server stores a credential hash.
An exact accepted retry returns the same receipt even after code expiry; changing
the accepted identity or credential is refused. A revoked or superseded pairing
cannot be restored by its old accepted request, even if the credential is reused.
Fresh explicit pairing can reconnect a revoked machine while retaining its host
identity. It cannot replace an existing active credential.

`POST machines/{host_id}/heartbeat` uses the helper's bearer credential and
`{schema:1,machine_id,name,os,arch,helper_version}`. Supported OS values are
`windows|macos|linux`; architectures are `x86_64|aarch64`. The reply contains
`{schema:1,observatory_id,host_id,state:"connected",access:["observe"],
heartbeat_seconds:15}`. With installation access, the helper also advertises
`installation_protocol:1` and the reply may carry one fixed installation request.
This credential controls only its own pairing, heartbeat, fixed installation
access and result routes; it cannot use Observatory administration APIs.

`POST machines/{host_id}/revoke {}` accepts `provision` or that machine's
credential and returns `{host_id,state:"disconnected"}`. It preserves saved
hosts, endpoints and agents. No heartbeat or old claim can restore revoked trust.
The helper retains its local identity and shows disconnection; it never silently
pairs again.

`GET hosts` adds `machine:{id,os,arch,helper_version,reported_name,access,state,
last_seen_at,paired_at,revoked_at}` to paired inventory records. Their transport is
`paired`, address is null, and installer roots remain empty. Observation-only
records have no recipes; an active installation grant plus helper protocol 1
projects only the fixed `paired-instance` recipe. `state` is
`online` for a heartbeat within 45 seconds, `offline` when no fresh heartbeat is
available, or `disconnected` after revocation. These observations do not change
host configuration revisions or certify agent state. Display labels remain
separately editable; transport/recipes cannot convert pairing into a command
grant. Active paired hosts must be revoked before deletion, and endpoint-reference
checks still apply.

Errors retain `{error:string}`: `401` invalid credentials, `403` revoked,
`404` unknown, `409` conflicting claim, `410` expired/cancelled, and `429`
attempt or capacity limit. A network error preserves the pending request and
credential; it does not establish whether a claim committed. Reconcile the exact
request instead of generating a replacement. Helper presence operates without
agent prompts, session access or an inbound connection to the computer. Installing
and using a remote agent additionally needs a direct shared-network route.

### Fixed setup on a paired computer

The local helper can explicitly upgrade an observation-only grant using
`POST machines/{host_id}/installation-access {install_agents:true}` with its
machine credential. This changes only that current pairing. No browser admin
credential can impersonate the local choice, and revocation still preserves agents.

`POST install/plan` uses `{host_id,recipe_id:"paired-instance",name,request_id}`.
The server pins the package target and SHA256 from `instance-bundle-lock.json`,
the current machine/pairing/Observatory identities, and the authenticated heartbeat
peer address. `POST install` adds the returned `plan_digest`. No request can select
an executable, argument, path, URL, credential endpoint or port. The helper checks
its own route to Observatory before running fixed Instance setup in its private
`agents/<request_id>` folder. The same operation is redelivered after reply loss;
a durable helper start without a result remains unknown and is never rerun.

`POST machines/{host_id}/installation-result` uses that machine credential and
`{request:<exact offer>,state:"completed"|"failed"|"unknown",message,attachment}`.
A completed attachment supplies the verified installed identity, network origin,
CA, credential and installation-owned workspace. These are retained privately;
only the result state and request digest enter the job record. Results must match
the current pairing and original job. Observatory verifies TLS and Instance
identity before registering management and the existing ACP attach bridge. The
bridge's local working directory and remote `session_cwd` are distinct.

Installed-but-unconnected results retain `connection_pending`; `POST jobs/{id}/connect`
finishes only that saved attachment. It does not reinstall, prompt, choose model
access or change the installed agent. Observation, package support, installed
state, usable connection and model authorization remain distinct outcomes.

### Owner installation recipes

`POST install/plan {host_id,recipe_id,name,home,port}` returns a revisioned plan and `plan_digest`. `POST install` takes the same fields plus that digest and `request_id:<UUID>`. One pending/running installation per host is allowed. `GET jobs`, `GET jobs/{id}`, `GET jobs/{id}/log/{stdout|stderr}` observe it. Logs are bounded to 1 MB each and require provisioning scope. The dispatcher stops the installer process group on timeout or control-connection loss, including descendants. Successful installers must register long-running services separately (for example, with systemd), rather than leave background children. Interrupted installations are never automatically rerun.

Keep the exact request ID and body if a reply is lost, and inspect that saved job
before another attempt. An identical repeated ID returns the existing job;
changing its body is rejected. `completed` records installer success, not endpoint
registration or model access. Connect and inspect the resulting installation
separately.

The complete desktop package registers a server-owned `native_local` host and
fixed `bundled-instance` recipe during first setup. Its plan uses
`{host_id, recipe_id, name, request_id}`; the server chooses a new per-user folder
and an available port. Browser-supplied paths and nonzero ports are refused. The
same request ID is used for planning and installation. The copied Instance
package remains installation-owned when the original download is removed.

After successful startup, Observatory verifies the returned Instance identity and
attaches its existing local management control. A completed job has `endpoint_id`
when that succeeds; model access is still independent. If verification is
incomplete, `connection_pending` preserves the installation. An authenticated
`POST jobs/{id}/connect` with provisioning scope retries only that exact saved
connection verification and registry save. It never repeats installation, starts
a model, or changes the agent's credentials. Generic recipes keep their existing
explicit connection workflow.

## Views, reports and links

`POST views`, `GET views/{id}`, `POST views/{id}/commands`, `POST views/{id}/ack`.

```json
{
  "expected_revision": 7,
  "controller": "voice-assistant",
  "commands": [
    {"op":"set","field":"screen","value":"agents"},
    {"op":"set","field":"selected_id","value":"endpoint-uuid"},
    {"op":"set","field":"tab","value":"work"}
  ]
}
```

Other fields are `session_id`, `filter`, `modal`, `draft`, `focus`, and `scroll:{x,y}`. The browser acknowledges the rendered revision with `{revision}`. Stale mutations return 409. A selected conversation must belong to the selected endpoint; contradictory commands fail atomically. Selecting another endpoint clears the previous conversation unless a matching conversation is explicitly supplied. Installation results can be opened with `modal: "job"` and `draft: {job_id}`. View acknowledgments and ordinary navigation are not activity events. Passwords and credential values are never form draft fields.

Set `screen: "world"` for the systems map. Its top-level `world` field stores the
layout and navigation; the selected agent still uses `selected_id`:

```json
{
  "camera":{"x":0,"y":0,"scale":1},
  "positions":{"machine:9c7b4074-0209-4d7d-9d9a-a7a0919689ab":{"x":160,"y":80}},
  "machine_id":null,"place_id":null,"room_id":null,"navigation":"world"
}
```

World members are optional; unknown members are rejected. Camera scale is
0.05–2.5; all coordinates must be finite and within ±1,000,000. At most 500 layout
positions are accepted, keyed by `machine:<UUID>`, `agent:<UUID>` or
`place:<opaque-key>`. Machine selection is a UUID; place and room keys use the
opaque-ID format above. Navigation is `world`, `machine`, `agent`, `place` or
`room`. Layout limits are not fleet capacity limits. These view commands only
change presentation; they cannot move installations or dispatch connection or
host operations.

`GET/POST links`, `PUT/DELETE links/{id}` describe `{label,from,to,relation,notes?}`, with relation `connects`, `uses`, `observes`, or `coordinates`. These are inventory descriptions, not a delegation authority or a network connection setup shortcut.

`GET/POST experiments`: `{label,run_id,status,summary,measurements,participants?,report_url?}`. Measurements can distinguish social participation, task outcomes, technical reliability, and actual human feedback. The service marks each as a submitted report; it does not independently certify the evaluator's claims.

`GET activity?limit=&before=` returns bounded operational events. `POST tokens {scopes,expires_in_seconds?}` returns a token once; `DELETE tokens/{id}` revokes it.
## Recorded experiment conversations

`PUT /api/v1/experiments/{id}/transcript` attaches or replaces a public transcript
on an existing experiment. It requires `manage` and a current
`expected_revision`. The body contains `messages`, an ordered array with `id`,
`speaker`, `kind`, `room`, `text`, and optional `at` (display timestamp). Kinds are
`simulated_human`, `support_agent`, `human`, `operator_cue`, `membership`, and
`protocol_fixture`.
Publishers must supply only contributions that were actually delivered, exclude
private drafts, tools, hidden reasoning and credentials, and label simulations
and operator cues honestly. Extra fields and unknown kinds are rejected.

Transcripts are bounded to 500 distinct messages, 128 KB per message and 1 MB of
message text total. An empty array removes the attached transcript.
`GET /api/v1/experiments/{id}/transcript` requires `observe` and returns the stored
revision and messages. The experiment index returns only `transcript_count`,
so opening the index does not send every transcript to the browser.

The packaged UI renders the transcript through the same Markdown renderer as
agent replies. Simulation and operator labels remain visible. It never executes
submitted report HTML. View commands can set `experiment_id` and
`experiment_measurements`; selection, disclosure, focus and scroll survive
reload. Existing CLI `api` commands can call both transcript endpoints.


## Agent questions, authentication, and optional session controls

Pending `elicitations` appear only for the initiating actor in `GET sessions/{id}`.
Answer through `POST sessions/{id}/elicitations/{request}` with
`{action: "accept"|"decline"|"cancel", content?: {...}}`. Flat primitive/enum
forms are validated against the agent's schema. URL requests display the full
address and open only after an explicit choice; opening does not prove that
external authentication completed. Values and authorization URLs stay out of
audit records. Expiry, cancellation, and restart cancel unanswered requests.

During session setup, AUTH_REQUIRED can offer supported agent authentication
methods. A chosen method runs `authenticate` and then retries setup once; prompts
are never replayed. Terminal-only authentication still requires the agent's
own setup flow. Auth callbacks remain attached to the initiating actor.

`POST endpoints/{id}/acp-sessions` requires `manage`. `{action:"list",cursor?:...}`
negotiates `sessionCapabilities.list`, filters to the configured working folder,
and returns bounded entries and an optional next cursor. `{action:"delete",
session_id:"...",confirm:true}` requires advertised delete support and explicit
confirmation. It removes an entry from the agent's list, not transcripts held by
this or other clients. Known active local sessions cannot be removed; external
clients may still be affected. Agent-specific retention remains the agent's
responsibility. Listing uses a separate connection and may require the agent's
persistent authentication to be configured first.

Closing an idle conversation calls `session/close` when advertised, releases the
client connection, and retains its transcript. Reopening prefers `session/resume`
when advertised, otherwise `session/load`. Unknown-session close/delete errors
are treated as already closed/removed. No unsupported methods are sent.

Views expose `agent_history_open`, `agent_history_cursor`,
`agent_history_delete`, and non-secret `elicitation_drafts`. Agents can drive
these controls through the same revisioned view API as other screens.

## Installation updates

For installations advertising `constellation.installation-updates/1`, the agent’s **Updates** tab exposes checks and installation requests. Preferences open the installation’s own programmable dashboard; its independent helper performs upgrades and recovery. Other ACP agents remain usable without this capability.
