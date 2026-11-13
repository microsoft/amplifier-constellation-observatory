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

Observatory and its computer helper target Windows 11 (Intel/AMD and ARM), macOS
(Intel and Apple silicon), and Linux (x64 and ARM64). Windows packages include the C runtime; Mac packages
use system libraries. Linux packages use the supported distribution libraries,
including PAM for existing machine-account installations. WSL, Rust, Python and
Node are not needed to open Observatory. The shared
[single-computer setup promise](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/installations.md#native-single-computer-setup)
also applies to the components people connect to it.

## Open on your computer

Download the Observatory package for your operating system and processor from
[Releases](https://github.com/microsoft/amplifier-constellation-observatory/releases).
Choose the **desktop** package to include local agent setup and computer-helper
downloads for all six supported platform/processor combinations. Keep its support
folder with the app until setup retains its own copy. Extract it, then open **Observatory** on Mac or **agent-observatory.exe** on
Windows. On Linux, open the extracted `agent-observatory` executable. The app opens
your browser and asks for a name and password for this local installation. You
can choose to start it when you sign in. Reopening the app returns to the same
installation; it does not create a second one.

To update, open a newer matching download and choose **Update and reopen**.
Observatory waits for its active conversations and installations to finish,
then replaces its own application files and included setup downloads. Your account, canvas, connections and
independently running agents remain in place. If startup fails, it restores the
previous application. Reopening after an interruption resumes that same update.
The optional start-at-sign-in choice is preserved.

Your computer appears on the canvas. Select **Create an agent**, name it, and
confirm installation. The included Instance package installs its private runtime
and setup support, starts the agent, and connects its verified identity to
Observatory. Model account access and service connections are the next steps;
installation does not create a conversation or prompt a model. A smaller server
package omits local agent setup and can connect existing installations.

This first setup listens only on your computer. It needs no certificate, remote
account or GPU. To pair another computer, **Add a computer → Use this network**
opens a separate HTTPS listener on the network address you select. Only computer
helpers use that listener; your browser and management controls stay local.
Existing HTTPS installations keep their configured address. Personal/team
membership is still a separate product direction.

Windows and Mac packages are unsigned previews until release signing and Mac
notarization are established. Native CI checks extracted-package setup, login,
restart and state preservation on both architectures. This does not replace
consumer installation qualification or remove operating-system launch warnings.
Do not disable system protections to run a package.

Running a local model or Minecraft server has its own hardware and platform
requirements. Opening Observatory does not install those services or change an
existing agent's engine, model, credentials or work.

## Advanced Linux installation

```sh
git clone https://github.com/microsoft/amplifier-constellation-observatory.git
cd amplifier-constellation-observatory
./install.sh --home "$HOME/.local/share/agent-observatory/default"
```

This developer/administrator installer builds a pinned checkout, creates one private installation, installs a user service, and prints its address. Rust, a C toolchain, libclang, and PAM development headers are required for this source build; on Ubuntu the script can install missing system packages. They are not prerequisites for the native packages above. Existing installations are never overwritten by this command.

Sign in using the configured machine account. A generated local certificate must be trusted by your browser; alternatively, put the HTTPS listener behind your existing trusted Tailscale HTTPS endpoint and use its DNS name. The server binds to all interfaces by default; choose `--bind 127.0.0.1` for a host-only listener. The generated port is stable after installation.

Desktop packages already include computer-helper downloads. For a server
installation without them, its owner can stage the published bundle after
verifying its release digest:

```sh
python3 scripts/stage-machine-helpers.py --home OBS_HOME --bundle constellation-machine-helpers-0.2.1.zip --sha256 VERIFIED_RELEASE_DIGEST
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

Choose **Add a computer**. For a local Observatory, select a shared LAN or private
network address and choose **Use this network**. Then download the setup for the
other computer and open it there.
Enter the short code shown in Observatory. The setup identifies this Observatory
and connects outward over HTTPS; the helper needs no SSH, central account or
inbound connection. Both computers still need a working route to Observatory,
and its firewall must allow the selected listener. Pairing does not create that
route or change firewall rules. Network settings show when the selected address
is unavailable; the local Observatory remains usable. Turning off this network
access pauses helper status without revoking saved pairings or stopping agents.
Only packages available and verified by this Observatory are offered.
The paired computer does not need Rust, Python or a developer toolchain. Windows
packages include their C runtime; Linux helper builds do not depend on the
Observatory server's C library version.

The helper keeps its own identity and starts for the signed-in user. A fresh
heartbeat shows that it is reachable; sleep or a network interruption preserves
the computer and its existing agents. A lost pairing reply can be reconciled
without creating another computer. Revoking the pairing stops its access while
leaving agents, connections and installation records intact.

With the current helper, pairing offers one choice to let Observatory add agents.
Then choose the computer, **Create an agent**, and give it a name. The helper
installs its pinned complete Instance package in a new private folder and
Observatory verifies and connects it. No SSH, keys, folder paths or developer
tools are needed. Existing observation-only pairings stay that way; open their
helper and choose **Allow agent setup** to upgrade that pairing once.

Agent access uses the computers' shared LAN or private-network route, including
an inbound HTTPS connection to the new agent. Pairing does not open firewall
rules or create a relay. If setup or connection is interrupted, its saved request
and files stay visible; an uncertain installer is never repeated automatically.
A completed installation can finish connecting without reinstalling. Model
access and connections remain the agent installation's own setup.

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
python3 scripts/test-prepare-app-release.py
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
