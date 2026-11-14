# Validation

Run the repository's automated checks from a clean checkout. The source fixtures
use fictional identities and temporary state. They do not contain installed
agents, personal profiles, account credentials, or exported conversations.

```sh
cargo fmt --check
cargo test --locked
cargo build --locked
npm ci
npx playwright install chromium
node tests/browser.mjs
node tests/delayed-view-browser.mjs
node tests/management-browser.mjs
node tests/intake-browser.mjs
node tests/markdown.mjs
cargo clippy --locked --all-targets -- -D warnings
```

See the test scripts and CI workflow for prerequisites and platform conditions.
Browser tests use disposable local services and require Playwright/Chromium.
Linux service tests may require PAM development headers; isolated command tests
also need the documented OS isolation prerequisites. Opt-in real account, model,
or messaging tests remain separate from deterministic checks.

The [Actions page](https://github.com/microsoft/amplifier-constellation-observatory/actions) records checks
against the published source. Earlier private deployment reports and their
transcripts are excluded. A passing fixture verifies its tested boundary, not
general agent quality, human approval, or live tenant compatibility.

The input-continuity suite uses real asynchronous ACP subprocesses and disposable
HTTPS services. It checks ordered/repeated intake, stable request IDs, final-turn
races, generic FIFO fallback, partial-frame reads, pending approvals, explicit
stop, queue limits, credential revocation/expiry, endpoint changes, restart
recovery, and unknown-delivery recovery without another prompt. Chromium tests
exercise send-while-busy, approval/draft preservation, refresh/retry after a lost
HTTP acknowledgment, explicit uncertain-delivery recovery, and a narrow layout.
These fixtures do not claim live-provider steering latency or model quality.

`context_fidelity` and `management_fidelity` check saved author identities,
endpoint-revision-bound observations, and confirmed versus unknown management
outcomes. The delayed-view Chromium fixture places polling between an edit and
its deferred save, proving old input cannot adopt a newer revision or resource.

`source_work` uses a deterministic ACP subprocess implementing the negotiated
Instance ordinary-source profile. It covers one lost prompt reply followed by
read-only inspection, no prompt or acknowledgment replay, transient inspection
ownership, noninteractive restoration, original-actor visibility, and nullable
or absent retained evidence. This is client-boundary coverage; it is not an
actual Instance/model or cross-component reference-journey claim.
