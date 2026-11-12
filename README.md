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
applies Converge to contributions. The default browser view shows computers,
their agents and the places those agents connect to. **Advanced controls** at
`/advanced` retain the detailed administration screens.

The sections below explain the installation and its controls. Discover the actual
endpoint's capabilities before relying on optional management behavior.

Observatory currently runs on Linux. Its computer helper targets Windows, macOS
and Linux, so those computers can pair with an Observatory over a reachable LAN
or private network. Pairing reports computer availability; it does not install
agents or authorize remote commands. Personal/team membership and cross-platform
Observatory hosting remain product directions.

## Install on Linux

```sh
git clone https://github.com/microsoft/amplifier-constellation-observatory.git
cd amplifier-constellation-observatory
./install.sh --home "$HOME/.local/share/agent-observatory/default"
```

The installer builds a pinned checkout, creates one private installation, installs a user service, and prints its address. Rust, a C toolchain, libclang, and PAM development headers are required for building; on Ubuntu the script can install missing system packages. Existing installations are never overwritten by this command.

Sign in using the configured machine account. A generated local certificate must be trusted by your browser; alternatively, put the HTTPS listener behind your existing trusted Tailscale HTTPS endpoint and use its DNS name. The server binds to all interfaces by default; choose `--bind 127.0.0.1` for a host-only listener. The generated port is stable after installation.

To offer computer-helper downloads, the Observatory installation owner stages
the published bundle after verifying its release digest:

```sh
python3 scripts/stage-machine-helpers.py --home OBS_HOME --bundle constellation-machine-helpers-0.1.0.zip --sha256 VERIFIED_RELEASE_DIGEST
```

This verifies the bundle and platform archives, retains the previous staged
files, and changes no service, agent or pairing. People adding their computers
download the tailored setup from Observatory; they do not run this owner command.

## Connect an agent

Open **Add → Connect an existing agent**. Supply its HTTPS management address
and credential, optionally choose its computer, then check the observed identity.
An unreachable agent stays saved with an unverified state. Computer placement is
explicit; a name or address does not establish where an agent runs.

Choose an agent to see its saved connections. Connect and Disconnect use that
installation's advertised controls; setup choices appear only when it offers
them. Place groupings describe shared workspaces, servers and their rooms.
Grouping does not authorize a connection or prove presence, and unavailable
connection status remains unknown.

For ACP-only and other implementations, use **Advanced controls → Connect
existing**. Supply the executable, arguments and working folder used by a
compatible ACP client. A remote agent can use an explicit SSH attach command.
Credentials belong in private secret references, never command-line arguments.

Built-in management mappings support Agent Instance, ACP Slack Adapter, and Persona Workshop. `generic-json` accepts a bounded list of explicitly configured operation mappings. An ACP-only endpoint remains usable without any management provider. Remote installation IDs are pinned before private reads or management writes. An optional `acp.session_cwd` selects the folder on a remote agent; `acp.cwd` is the local attach process folder.

## Pair a computer

Choose **Add a computer**, download the setup for that computer, and open it there.
Enter the short code shown in Observatory. The setup identifies this Observatory
and connects outward over HTTPS; no SSH, central account or inbound firewall rule
is needed. Both computers still need a working network route to Observatory.
Only packages available and verified by this Observatory are offered.
The paired computer does not need Rust, Python or a developer toolchain. Windows
packages include their C runtime; Linux helper builds do not depend on the
Observatory server's C library version.

The helper keeps its own identity and starts for the signed-in user. A fresh
heartbeat shows that it is reachable; sleep or a network interruption preserves
the computer and its existing agents. A lost pairing reply can be reconciled
without creating another computer. Revoking the pairing stops its access while
leaving agents, connections and installation records intact.

Helper build support is distinct from consumer installation qualification.
Windows and macOS packages are not yet signed/notarized; platform launch warnings
and startup behavior must be verified on the intended OS. Do not disable system
security to make a package run. The helper shows startup failures rather than
claiming it will remain available after sign-in.
The unsigned Mac preview carries public setup inside its app so moving the app
does not lose the Observatory address. Signed distribution must use a separately
opened setup file rather than modifying a signed bundle.

Existing local and SSH hosts remain available under advanced setup. Saving those
inventory entries does not install the helper or test access. **Create an agent**
uses one of that computer's approved installation recipes; configure recipes and
allowed folders under Advanced controls. Pairing alone provides no such recipe.
Each plan binds the selected recipe and host revision.
Saved job results survive a lost browser response. After an installer finishes,
complete its own setup and connect its endpoint here. Recipes can install any
implementation; they do not need to install Agent Instance.

For optional update controls, see [installation updates](docs/api.md#installation-updates).

## Programmatic use

```sh
agent-observatory --home /path/to/observatory status
agent-observatory --home /path/to/observatory api GET endpoints
agent-observatory --home /path/to/observatory api POST endpoints --body-file endpoint.json
```

The browser, CLI and other agents use the same `/api/v1` operations. Screens,
selection, ordinary form drafts, focus and canvas positions use revisioned view
state. Moving a canvas item changes its layout, not its installation or
connections. Secret values use a separate write-only API. Embedding Observatory
requires explicit HTTPS `embed_origins` in its installation config. Embedded
agent dashboards retain their own authentication and embedding policy.

See the [API reference](docs/api.md) for requests, scopes, examples and compatibility limits.

## Checks

```sh
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo build --locked
cargo test --manifest-path machine-helper/Cargo.toml --locked
python3 scripts/test-machine-helper-packaging.py
npm ci
npx playwright install chromium
npm test
node tests/management-browser.mjs
node tests/world-model.mjs
node tests/world-browser.mjs
node tests/machine-pairing-browser.mjs
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
