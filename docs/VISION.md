# Observatory vision

People should be able to understand and operate several independent agents
without becoming an expert in every installation. Observatory brings their
exposed information and controls together so an operator can find the right
individual, see what needs attention, inspect work, and take an appropriate
next action.

Its value is convenient oversight with dependable boundaries. An agent can live
on another machine, use a different engine, or expose only ordinary Agent Client
Protocol (ACP). Richer management capabilities should improve the experience
when available without making Observatory a prerequisite for the agent's life.

## Help people make consequential decisions

The operator should be able to distinguish an unavailable endpoint from an idle
agent, accepted work from verified results, and a saved request from an uncertain
delivery. The next useful step should be understandable without reading a stream
of raw events. Details and source evidence should remain accessible when they
matter to a decision.

Observatory should support both direct interaction and management of independent
installations. Those are different responsibilities: talking to an agent does not
mean administering its host. A coherent experience can present both while keeping
their meaning clear.

## Compose through owned interfaces

Observatory is a client of agent and installation interfaces. The services being
observed remain responsible for their identity, configuration, credentials,
work and effects under the [family agreement](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/family.v1-candidate.md).
Its own registry, connection settings and observations support that composition;
they do not replace those services' authoritative records.

This creates room for different forms of oversight: a focused operator workspace,
a broader experience, or automation using the same exposed operations. The
current browser control panel is development and administration access, not a
commitment to the final product design.

## Judge the whole outcome

Success means that people can discover a problem, understand the evidence,
choose an action within their authority, and see its actual outcome with less
coordination effort. Count setup, investigation, correction and recovery effort
as well as the visible interaction. A connected endpoint or successful probe
alone does not establish that result.

The [public boundaries](contracts.md) own Observatory's concrete interfaces.
The [family contract map](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/README.md)
locates shared promises. Use current behavior and meaningful checks to choose the
next improvement; the vision does not maintain a feature or gap inventory.
