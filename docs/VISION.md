# Your Constellation, within reach

Observatory makes the systems behind your agents understandable and manageable.
See what you have, where it runs, how it connects, whether it is working and what
you can add or change, without learning each component's installation mechanics.

## See how the pieces fit

See machines containing independent agent instances, with an engine inside each
instance. Follow connections out to services and conversations. An authorized
Slack workspace, an installed connector and an agent connected to a particular
channel are different things; the view should make their relationship clear.

Select a part for its configuration, observed health and supported actions.
Distinguish offline, still being set up and ready now without reading logs.
The first slice is this systems view and setup controls. A work screen, task
tracking and an attention inbox are deferred.

## Start on one computer, grow across machines

The target is ordinary Windows, Mac and Linux computers. A person can begin with
Observatory and agents on one device, or run Observatory on an always-on machine
and reach it from a browser over their LAN or a private network such as Tailscale.
Model execution and services have their own hardware needs; observing and managing
them should not require an AI workstation.

Adding a machine should feel like installing a familiar app and pairing it once.
Reuse permitted working access; ask only for what is missing. A proposed host
helper would reuse existing installation machinery. Routine connection details
stay out of the way; the intended machine and granted access stay understandable.

From there, deploy an agent, connect an existing one, authorize a service, or
connect it to a conversation. Where domain tooling supports it, create a service
such as a Minecraft server as an alternative to selecting an existing server.
Keep completed setup visible when a later step fails and help the person continue
from the actual state.

## Personal first, shared when useful

Personal and team use belong to one product. Invite collaborators and share
selected parts without rebuilding the setup. Team use adds membership, permissions,
administrative history and ownership transfer. Returning to personal use resolves
shared ownership and access while preserving records and team resources.

Observatory remains an optional client of independent installations. The
[shared agreements](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/README.md)
keep state, authority and lifecycle with their owners. The
[local agreement](contracts.md) defines Observatory's contribution; the
[API](api.md) and [installation instructions](../README.md#install-on-linux)
describe implemented support. Platform targets, pairing and team flows here guide
development; they are not claims that today's reference control panel implements
them.
