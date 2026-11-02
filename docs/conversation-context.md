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

This interface does not supply backend-only working transcripts, authorize
external actions, or grant fleet management. ACP-only agents can still connect
without it. Adapters for other implementations may consume this optional source
contract or expose it through an MCP server they control.
