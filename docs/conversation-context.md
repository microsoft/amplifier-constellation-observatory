# Conversation continuity

Talk uses ordinary ACP. Context remains an optional agent-owned capability,
independent of the management API. A private owner attach credential for Agent
Instance should carry `acp` and `context:owner`; ACP access alone must not imply
permission to read other conversations. Shared-room clients use room grants.

To let an agent retrieve its Observatory exchanges, issue an Observatory token
through `POST /api/v1/tokens` with `scopes: ["context:<endpoint UUID>"]`. It is
read-only and limited to that registered endpoint's conversations. Register
`https://<observatory>/agents/<endpoint UUID>` as a conversation provider URL
with this token in a named secret. The source accepts the existing
`POST /context/v1/query` contract for owner access, list/read/search/orientation.
Responses carry links, timestamps and completeness information. Other agents'
conversations are not returned. Revoke through the ordinary token DELETE API.

List and read results identify the saved `endpoint_id` and conversation
`revision`. Message IDs remain the original conversation/message position;
timestamps and links refer to that saved source. A message's `actor` is its
recorded author, not the installation owner or the endpoint's current display
label. Legacy messages without an author return null rather than an inferred
identity. Assistant messages may also include `actor_label` for the endpoint's
current label; that label is presentation, not authorship.

When present, `request_id`, `sequence`, `delivery` and `acknowledged` preserve the
saved input facts. They are not accepted-work IDs or proof of effects. The text
budget, truncation flag and `complete` field still describe the returned portion
of this source. Missing history or a truncated response is not evidence that no
other messages or work exist. Private credential references and internal session
state are not projected.

## Optional source and work references

For an agent advertising the complete ordinary-input source-delivery profile,
Talk binds an original input to its source ID, revision and timestamp before
dispatch. An authorized original actor can explicitly inspect that input through
[`POST /api/v1/sessions/{id}/inputs/{request}/inspect`](api.md#optional-ordinary-input-source-and-work-inspection).
This uses the same saved remote session and endpoint. It sends no new prompt,
acknowledgment or queue-resume instruction, and does not create work. Generic ACP
agents and existing context clients need not support the optional profile.

A saved message can carry private `source_observation` data, containing the last
successful receipt query and work references. Session responses expose it only
to the original input actor. The context interface deliberately omits it:
endpoint-scoped context access does not inherit that actor's receipt access or
interactive authority. The input facts above remain available without querying
the remote agent or exposing its private work references.

The private inspection API distinguishes Observatory's local `observed_at` from
nullable remote `receipt.observedAt`, and null/empty/partial evidence from proof
of absence. Its saved references are observations rather than current status or
accepted-work authority. Use the agent owner's authorized work interface for
current status, acceptance, amendment or cancellation. Neither context retrieval
nor receipt inspection settles an unknown effect or permits replay.

This interface does not supply backend-only working transcripts, authorize
external actions, or grant fleet management. ACP-only agents can still connect
without it. Adapters for other implementations may consume this optional source
contract or expose it through an MCP server they control.
