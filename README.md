# Agent Observatory

Part of [Amplifier Constellation](https://github.com/microsoft/amplifier-constellation).

An optional home for seeing and managing the systems behind your agents:
machines, instances, engines, services and connections. Connect through ACP or
an installation's declared management interface across different implementations.

Start with the [vision](docs/VISION.md) for intended value and the
[public boundaries](docs/contracts.md) for Observatory's interface commitments.
The [Constellation family agreement](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/composition.md)
owns shared composition and state ownership; the
[working guide](https://github.com/microsoft/amplifier-constellation/blob/main/docs/working-together.md)
applies Converge to contributions. Existing browser screens are reference
administration access; future experiences can use the same service boundaries.

The sections below explain the installation and its controls. Discover the actual
endpoint's capabilities before relying on optional management behavior.

The product direction targets personal and team use on Windows, macOS and Linux,
with approachable machine pairing. The implementation described below currently
uses Linux hosting and explicit local/SSH installation recipes; those future
flows are not yet installation options.

## Install on Linux

```sh
git clone https://github.com/microsoft/amplifier-constellation-observatory.git
cd amplifier-constellation-observatory
./install.sh --home "$HOME/.local/share/agent-observatory/default"
```

The installer builds a pinned checkout, creates one private installation, installs a user service, and prints its address. Rust, a C toolchain, libclang, and PAM development headers are required for building; on Ubuntu the script can install missing system packages. Existing installations are never overwritten by this command.

Sign in using the configured machine account. A generated local certificate must be trusted by your browser; alternatively, put the HTTPS listener behind your existing trusted Tailscale HTTPS endpoint and use its DNS name. The server binds to all interfaces by default; choose `--bind 127.0.0.1` for a host-only listener. The generated port is stable after installation.

## Connect an agent

Use **Connect existing**. Add a name and, if available, its management address and a named credential stored through **Access**. Under ACP options, supply the executable, arguments, and working folder used by any compatible ACP client. A remote agent can supply an explicit SSH or other attach command. Credentials belong in private secret references, never in command-line arguments.

Built-in management mappings support Agent Instance, ACP Slack Adapter, and Persona Workshop. `generic-json` accepts a bounded list of explicitly configured operation mappings. An ACP-only endpoint remains usable without any management provider. Remote installation IDs are pinned before private reads or management writes. An optional `acp.session_cwd` selects the folder on a remote agent; `acp.cwd` is the local attach process folder.

**Machines** holds separately authorized local/SSH installation recipes. A recipe names an installer version, argument array, and allowed installation roots. A plan has a digest and host revision; stale plans fail. Install jobs retain their exit result and private output. A successful installer exit is not proof that the new agent is ready: connect and inspect its reported endpoint next. Recipes may install any implementation; they do not need to install Agent Instance.

For optional update controls, see [installation updates](docs/api.md#installation-updates).

## Programmatic use

```sh
agent-observatory --home /path/to/observatory status
agent-observatory --home /path/to/observatory api GET endpoints
agent-observatory --home /path/to/observatory api POST endpoints --body-file endpoint.json
```

The browser, CLI and other agents use the same `/api/v1` operations. Every visible screen, selected endpoint, conversation, filter, modal, ordinary form draft, focus, and scroll position has a revisioned view representation. Secret values are submitted through a separate write-only API. Embedding the Observatory itself requires explicit HTTPS `embed_origins` in its installation config. Embedded agent dashboards retain their own authentication and embedding policy.

See the [API reference](docs/api.md) for requests, scopes, examples and compatibility limits.

## Checks

```sh
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo build --locked
npm ci
npx playwright install chromium
npm test
node tests/management-browser.mjs
```

Tests use disposable state, real HTTPS, real browser interactions, and deterministic ACP subprocesses. They do not post into a real collaboration service. Browser artifacts are private and ignored by Git.

For experiment transcripts, see [recorded experiment conversations](docs/api.md#recorded-experiment-conversations).

## Contributing

> [!NOTE]
> This project is not currently accepting external contributions, but we're actively working toward opening this up. We value community input and look forward to collaborating in the future. For now, feel free to fork and experiment!

Most contributions require you to agree to a
Contributor License Agreement (CLA) declaring that you have the right to, and actually do, grant us
the rights to use your contribution. For details, visit [Contributor License Agreements](https://cla.opensource.microsoft.com).

When you submit a pull request, a CLA bot will automatically determine whether you need to provide
a CLA and decorate the PR appropriately (e.g., status check, comment). Simply follow the instructions
provided by the bot. You will only need to do this once across all repos using our CLA.

This project has adopted the [Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/).
For more information see the [Code of Conduct FAQ](https://opensource.microsoft.com/codeofconduct/faq/) or
contact [opencode@microsoft.com](mailto:opencode@microsoft.com) with any additional questions or comments.

## Trademarks

This project may contain trademarks or logos for projects, products, or services. Authorized use of Microsoft
trademarks or logos is subject to and must follow
[Microsoft's Trademark & Brand Guidelines](https://www.microsoft.com/legal/intellectualproperty/trademarks/usage/general).
Use of Microsoft trademarks or logos in modified versions of this project must not cause confusion or imply Microsoft sponsorship.
Any use of third-party trademarks or logos are subject to those third-party's policies.
