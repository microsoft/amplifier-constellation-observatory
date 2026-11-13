# Fleet observation and management

<a id="public-boundaries"></a>

For people managing their systems, clients and management-provider authors.
The promise: Observatory provides a systems-management
[Experience](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/experience.md)
through independently owned interfaces. The five primitive boundaries and shared
state/access rules remain in [composition](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/composition.md).

**Example:** A person pairs another computer, sees the installations it exposes,
and connects one agent to a service. A later network failure shows which step
completed and which connection is uncertain. The setup remains available to
reconcile rather than being repeated from the beginning.

These commitments guide the systems experience in the [vision](VISION.md).
The [API reference](api.md) defines currently supported operations; a product
commitment does not advertise an unimplemented interface.

## Observatory's commitments

1. **Connect the interfaces independently.** An endpoint can expose ACP,
   management, a dashboard or a combination. Agent and adapter entries remain
   distinct; an inventory relationship neither connects them nor grants access.
   Check each attached interface and preserve its own timestamp, checked revision
   and outcome. Distinguish machine placement, advertised internal capabilities,
   service authorization and active conversation connections. Engine details apply
   only where the agent exposes them; an Agent Instance need not contain one engine.
   An ACP-only agent remains usable without machine or engine disclosure. One failing
   endpoint does not prevent inspecting the rest. Preserve each source's hierarchy: Slack
   workspaces and channels, Teams teams and channels or chats, Minecraft servers
   and their supported conversation spaces. A conversation connection shows
   participation by the existing agent under
   [composition](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/composition.md),
   without implying the agent or its execution has moved or been copied.
2. **Pin the intended installation.** Verify the remote installation identity
   before private management reads or writes. Observatory credentials and remote
   credentials each constrain access. Read-only observation does not imply
   prompting, private work access, configuration or host provisioning. Machine
   pairing establishes scoped, revocable trust once per Observatory; ordinary
   use within that grant does not require repeated pairing. Discovery alone
   establishes neither trust nor reachability.
3. **Use explicit management mappings.** A registered provider selects supported
   operations and their required access. Callers cannot choose arbitrary remote
   URLs, and unsupported features do not fall back to private files. Optional work,
   memory, updates and model controls retain their distinct capabilities.
4. **Keep attached execution sessions recognizable.** Where ACP sessions are exposed,
   preserve endpoint/session association and the initiating actor's pending decisions.
   Advertised resume/load may restore the execution supporting an exchange;
   otherwise report the limitation instead of claiming that a new prompt recreates
   the old session. Endpoint-owned tools and retention remain
   the endpoint's responsibility.
5. **Keep machine installation a separate grant.** Explicit host recipes pin the
   installer, permitted roots and arguments. Plans bind the host revision and
   request identity. Record installer outcomes and interruption without automatic
   reruns; a successful installer exit still needs endpoint readiness checking.
6. **Show observations as observations.** Provider reports and submitted experiment
   claims are attributed, not independently certified. Published experiment
   transcripts contain delivered material with simulation/operator labels, not
   private drafts or hidden reasoning. View state can select evidence and controls;
   selecting a view does not perform their actions. Show desired state separately
   from observed state and its freshness; a sleeping or unreachable machine is
   not evidence by itself of a successful stop, uninstall or failed installation.
7. **Make setup approachable through owned operations.** Target local Windows,
   macOS and Linux use, plus LAN and private-network access. The local
   host helper connects the computer without replacing supported installers or
   lifecycle controls; it is not an
   arbitrary host-command API or a prerequisite for connecting an existing agent.
   Offer domain provisioning only through its owner's usable setup path. Retain
   completed steps and distinguish disconnect, uninstall and data deletion.
   Transport, discovery, browser trust and helper APIs require implementation
   qualification; this agreement does not define them.
8. **Grow membership without silently expanding access.** Personal and team use
   share the same product. Adding members preserves installations and records;
   the owner chooses which resources and supported operations become shared.
   Membership does not copy personal credentials or grant access to every paired
   machine. Support administrative history, ownership transfer and departing
   members' access revocation. Returning to personal use resolves team ownership
   and credentials while retaining history and accounting for resources that
   cannot transfer.
9. **Keep growing setups readable.** Group agents by their computer and let people
   inspect each agent and its connections. Show the full count and identify a
   filtered or partial view; an agent outside the visible group must not appear
   absent or disconnected. A visual group size is not a machine capacity limit.

## Shared promises and exact interfaces

[Composition](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/composition.md),
[interaction](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/interaction.md),
[work continuity](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/work-continuity.md),
[installations](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/installations.md)
and [engine compatibility](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/engine-compatibility.md)
own the shared promises. The [API reference](api.md) owns exact routes, scopes,
revision checks, callbacks, limits and view fields. These are capabilities for
clients, not a prescribed future screen layout.

**Check:** Connect an ACP-only agent and a managed installation. Change an endpoint
revision, fail one probe, revoke a credential and attempt a mismatched identity.
Exercise an actor-bound approval and an interrupted install. Confirm independent
observations, explicit unsupported results and no manufactured success. Walk a
first-time user through pairing, deployment and service connection, then a lost
connection and recovery. Test personal-to-team and team-to-personal transitions
with an unshared resource, a departing member and an interrupted administrative
operation; verify no implicit sharing, lost records or duplicate installation. See
[validation](validation.md) for executable checks. With a dozen or more agents on
one computer, including a partial or filtered view, locate an agent and trace its
participation in two conversations without confusing residence, identity or
connection state.

**Left open:** Systems-map layout, additional helper operations, role design,
sign-in provider, discovery, recovery UX and additional provider mappings.
The first product slice excludes work tracking and an attention inbox; existing
optional interfaces remain valid without dictating that product scope.
Observatory is optional; it does not introduce shared identity, provisioning or
execution requirements for the agents it observes.

## Interface section links

Existing section links lead to their exact reference below.

<a id="endpoint-registry"></a>
[Endpoint registry](api.md#endpoint-registry)

<a id="optional-memory-continued-work-and-engine-settings"></a>
[Optional memory, continued work and engine settings](api.md#optional-memory-continued-work-and-engine-settings)

<a id="conversations"></a>
[Conversations](api.md#conversations)

<a id="machines-and-installers"></a>
[Machines and installers](api.md#machines-and-installers)

<a id="views-reports-and-links"></a>
[Views, reports and links](api.md#views-reports-and-links)

<a id="recorded-experiment-conversations"></a>
[Recorded experiment conversations](api.md#recorded-experiment-conversations)

<a id="agent-questions-authentication-and-optional-session-controls"></a>
[Agent questions, authentication, and optional session controls](api.md#agent-questions-authentication-and-optional-session-controls)

<a id="installation-updates"></a>
[Installation updates](api.md#installation-updates)
