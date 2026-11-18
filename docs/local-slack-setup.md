# Local agent and Slack setup

Observatory is a client of two independent installations: the agent and its Slack
connector. It does not become their identity, credential or execution service.
The local setup path is offered only for an unchanged agent installed by this
Observatory's bundled Instance recipe and a reviewed Slack package for the
current computer. An empty package lock or unsupported platform disables setup.

## Model access and starting the agent

The model screen reads the installed Instance's advertised account, engine
configuration and schema APIs. It preserves an existing engine, provider, model,
instructions and tools. Saving changes is explicit and binds both the endpoint
revision and the Instance-owned configuration/backend revisions. API keys are
write-only, and provider authorization links are transient. No model prompt is
sent to test an account automatically.

Native account sign-in with a loopback callback is offered only when the browser
is on the same computer. A remote browser is not told that it can complete a
callback on the agent's host. Supported device-code flows retain their own rules.

**Start my agent** first completes the existing Instance setup flag through its
full configuration compare-and-swap, then explicitly starts it through its own
lifecycle API. Existing enabled installations are observed without another
start. A lost reply records the intended configuration hash and is reconciled by
reads; it never chains another mutation after an uncertain reply. A configured
account is not proof of an actual successful model response.

## Fixed local Slack adapter

The administrator-only `GET/POST /api/v1/endpoints/{id}/slack-setup` is a closed
client adapter, not an arbitrary command or filesystem API. Browser mutations
also require the normal CSRF protection. The browser can select an eligible
endpoint but cannot supply a package URL, executable, environment or home.

`prepare` requires a fresh UUID and observed setup revision. The durable record
is saved before download, installation, startup or configuration. The compiled
package lock binds the official archive, exact target, source, manifest and file
inventory. Only a newly created private package directory and new connector home
are used; existing installations are never adopted or repaired by this route.

The connector owns finite installation and its independent OS-managed startup.
Observatory invokes only the fixed native setup/start/status and owner-local API
commands. Its private control credential remains in the connector. The CLI proves
the local listener before using that credential; Observatory does not weaken its
ordinary remote HTTPS provider checks or expose the credential to the browser.

The request's exact installation and startup identity must be observed ready.
After a lost startup reply, a matching read-only observation may offer an explicit
`bind` step; it does not repeat startup or silently continue configuration. Agent
binding uses the pinned installed ACP command and workspace, preserving all other
connector defaults and credentials. Native readiness binds the saved request,
fixed manager profile and complete witnessed process generation. A stopped
owned connector can offer **Start saved connection**: an explicit new start of
the same installation, never a reinstall or an automatic response to a read.

`authorize`, `confirm`, `connect` and `discard` use the connector's existing Slack
setup API. Each is an explicit offered step with the saved operation/revision.
The authorization command and confirmation code remain transient. Reopening
setup reads the same current approval command from the connector; it does not
request another ticket or save that command in Observatory. An exact known
confirmation refusal is shown as a completed response, with another approval
remaining explicit. Unrecognized or lost responses remain uncertain. The authorized
workspace is displayed for confirmation before app creation. A workspace mismatch
cannot become a connection. Workspace administrator consent and Slack's platform
limits still apply; passing local fixtures does not grant that consent.

`GET /api/v1/slack-setups` exposes only the current actor's bounded saved
projections. It makes no connector request. The canvas labels these as previous
observations; opening setup checks the current installation. A generic running
observation cannot settle an unrelated uncertain action. Duplicate preparation
returns the original saved operation and never creates a second connector.

A parsed request rejected before any durable intent can return a correlated
`slack_setup_refusal` projection. It identifies the exact endpoint revision,
action, request or operation, and expected setup revision. The browser clears
only that rejected intention and reads current state; another action remains
explicit. Generic HTTP errors, failed persistence and uncertain connector
responses do not establish refusal or permission to repeat work.

## Qualification boundaries

Pure tests and browser fixtures cover configuration preservation, explicit
actions, closed dialogs, endpoint changes, lost replies, workspace mismatch and
returning to saved setup. The combined browser fixture uses the real desktop
account and world UI with synthetic Instance/Slack external boundaries. It gives
the Observatory application an OS-only PATH and records that limitation.

Native package tests separately cover actual installation and process ownership.
Windows and macOS qualification must verify the exact shipped cohort, hidden
helper launches, connector survival after Observatory exits and cleanup of only
the fixture-owned installations. Real provider and Slack consent plus a first
conversation require their separate authorized account test. No one test category
is relabelled as the others.
