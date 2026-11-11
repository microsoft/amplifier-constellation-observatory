# Fleet observation and management

<a id="public-boundaries"></a>

For clients, management-provider authors and operators. The promise: Observatory
combines useful access to independent agents without becoming the owner of their
state or confusing an observation with a successful action.

**Example:** An operator sees an agent needs attention, opens its responsibility
record and invokes a supported control. The agent's installation remains the
authority; the fleet view records what it observed and what the operation returned.

## Observatory's commitments

1. **Connect the interfaces independently.** An endpoint can expose ACP,
   management, a dashboard or a combination. Agent and adapter entries remain
   distinct; an inventory relationship neither connects them nor grants access.
   Check each attached interface and preserve its own timestamp, checked revision
   and outcome. One failing endpoint does not prevent inspecting the rest.
2. **Pin the intended installation.** Verify the remote installation identity
   before private management reads or writes. Observatory credentials and remote
   credentials each constrain access. Read-only observation does not imply
   prompting, private work access, configuration or host provisioning.
3. **Use explicit management mappings.** A registered provider selects supported
   operations and their required access. Callers cannot choose arbitrary remote
   URLs, and unsupported features do not fall back to private files. Optional work,
   memory, updates and model controls retain their distinct capabilities.
4. **Keep agent sessions recognizable.** Preserve endpoint/session association
   and the initiating actor's pending decisions. Advertised resume/load may restore
   a conversation; otherwise report the limitation instead of claiming that a new
   prompt recreates the old session. Endpoint-owned tools and retention remain
   the endpoint's responsibility.
5. **Keep machine installation a separate grant.** Explicit host recipes pin the
   installer, permitted roots and arguments. Plans bind the host revision and
   request identity. Record installer outcomes and interruption without automatic
   reruns; a successful installer exit still needs endpoint readiness checking.
6. **Show observations as observations.** Provider reports and submitted experiment
   claims are attributed, not independently certified. Published experiment
   transcripts contain delivered material with simulation/operator labels, not
   private drafts or hidden reasoning. View state can select evidence and controls;
   selecting a view does not perform their actions.

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
observations, explicit unsupported results and no manufactured success. See
[validation](validation.md) for executable checks.

**Left open:** Fleet layout, presentation and additional provider mappings.
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
