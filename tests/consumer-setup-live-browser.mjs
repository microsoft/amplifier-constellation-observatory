// Actual packaged main-UI onboarding. No synthetic HTTP responses or product hooks.
// Invoked only by consumer_setup_live.py after exact package/tool/cleanup gates.
// Stops before external provider/Slack consent. Canary credentials are not valid.
// Uses the installation's fixed native manager; never an external service kill.
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
import {createHash, randomUUID} from 'node:crypto';
import {execFile, spawn} from 'node:child_process';
import {promisify} from 'node:util';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {pathToFileURL} from 'node:url';

// A direct driver invocation must not bypass the Python pre-dispatch gate.
// Remove only with the exact supported lifecycle/retirement implementation;
// no command-line or environment override can turn incomplete coverage green.
const frozenSlackLifecycleContract = 'com.microsoft.amplifier.slack-native-status/1';
assert(frozenSlackLifecycleContract,
  'Native Slack generation and checked retirement contract is not frozen; no product was launched');

const input = JSON.parse(await fs.readFile(process.argv[2], 'utf8'));
assert.equal(input.schema, 1);
assert(path.isAbsolute(input.home) && path.dirname(input.home) === input.root);
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
assert.equal(sha(await fs.readFile(input.binary)), input.binary_sha256);
assert(['direct_payload', 'mac_launch_services'].includes(input.launch_mode));
const macLaunch = input.launch_mode === 'mac_launch_services';
if (macLaunch) {
  assert.equal(process.platform, 'darwin');
  assert.equal(input.app, path.join(input.root, 'download/Observatory.app'));
  assert.equal(input.gui, path.join(input.app, 'Contents/MacOS/Observatory'));
  assert.equal(sha(await fs.readFile(input.gui)), input.gui_sha256);
}
const {chromium} = await import(pathToFileURL(input.playwright_module).href);
const {DatabaseSync} = await import('node:sqlite'); // qualification Node >=22.13; not a product dependency
const output = name => path.join(input.evidence, name);
const password = 'Disposable local fixture ' + randomUUID();
const canary = 'fixture-only-not-a-provider-credential-' + randomUUID();
const observations = [], requests = [], browserErrors = [], external = [];
const stopDispatches = new Set();
let child, browser, context, page, base, csrf, endpoint, plan, beforeInstance, beforeRuntime;
let slackBefore, slackAfter, parentExited = false, completed = false;
let slackCleanup, instanceCleanup;
const retainedFixtureState = JSON.stringify({fixture_only:true, name:'existing local state', value:randomUUID()});
let stdout = '', stderr = '';
let launchNumber = 0, macWitness, macPayload;
const macLogs = [], macGenerations = [], macStops = [], macEvidence = [];

async function checkpoint(phase, detail = {}) {
  observations.push({phase, at: new Date().toISOString(), ...detail});
  await fs.writeFile(output('progress.json'), JSON.stringify({schema: 1, observations}, null, 2));
}
// Closed, content-free projection of the reply to this fixture's one prepare.
// Never retain messages, private records, auth commands, workspace data or bodies.
function prepareResponseProjection(value, request, selected) {
  const member = (value, allowed) => allowed.includes(value) ? value : null;
  const operation = value?.operation;
  const bound = value?.schema === 1 && value.endpoint_id === selected.id
    && value.endpoint_revision === selected.revision;
  const requestBound = typeof request?.request_id === 'string'
    && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(request.request_id)
    && request.action === 'prepare' && request.endpoint_revision === selected.revision;
  const refusal = bound && requestBound && value.kind === 'slack_setup_refusal'
    && value.refused_before_intent === true && value.action === 'prepare'
    && value.request_id === request.request_id && value.operation_id === null
    && value.expected_revision === request.expected_revision;
  return {
    endpoint_matches: bound,
    operation_matches: bound && requestBound && operation?.id === request.request_id
      && operation?.request_id === request.request_id,
    phase: member(operation?.phase, ['prepare', 'package', 'native-setup', 'native-start', 'bind-agent', 'bind']),
    state: member(operation?.state, ['running', 'complete', 'unknown']),
    uncertain: typeof operation?.uncertain === 'boolean' ? operation.uncertain : null,
    binding_verified: value?.connector?.binding_verified === true,
    capabilities: Object.fromEntries(['prepare', 'bind', 'resume', 'claim', 'authorize', 'confirm', 'connect', 'discard']
      .map(name => ['can_' + name, value?.capabilities?.['can_' + name] === true])),
    refusal_code: refusal ? member(value.code, ['endpoint_changed', 'setup_changed', 'invalid_input',
      'package_unavailable', 'existing_home', 'not_prepared', 'step_unavailable']) : null,
  };
}
async function openGuard() {
  try { await fs.stat(path.join(input.root, 'fixture-abort')); }
  catch (error) { if (error.code === 'ENOENT') return; throw error; }
  throw Error('Outer observer stopped the sequence; no later action is authorized');
}
async function absent(file) {
  try { await fs.lstat(file); return false; }
  catch (error) { if (error.code === 'ENOENT') return true; throw error; }
}
async function privateJson(file) { return JSON.parse(await fs.readFile(file, 'utf8')); }
async function verifyFixtureObserver() {
  assert.equal(sha(await fs.readFile(input.fixture_python)), input.fixture_python_sha256);
  assert.equal(sha(await fs.readFile(input.fixture_controller)), input.fixture_controller_sha256);
}
async function directoryIdentity(file) {
  if (process.platform === 'win32') {
    // libuv and Python can expose different widths for a Windows volume serial.
    // Both the initial witness and the later cleanup must compare the full FILE_ID_INFO.
    await verifyFixtureObserver();
    const {stdout} = await promisify(execFile)(input.fixture_python,
      ['-I', '-B', '-X', 'utf8', input.fixture_controller, '--windows-directory-identity', file],
      {env: input.application_env, windowsHide: true, timeout: 10_000, maxBuffer: 4096, encoding: 'utf8'});
    const identity = JSON.parse(stdout);
    assert.match(identity.volume_serial_64, /^[0-9a-f]{16}$/);
    assert.match(identity.file_id_128, /^[0-9a-f]{32}$/);
    return identity;
  }
  const value = await fs.lstat(file, {bigint:true});
  assert(value.isDirectory() && !value.isSymbolicLink());
  return {device: String(value.dev), inode: String(value.ino)};
}
async function api(resource, body, method = 'POST') {
  await openGuard();
  const response = await context.request.fetch(base + '/api/v1/' + resource, {
    // Keep the exact browser owner. The installation-local token is a different
    // actor and cannot read/continue this owner's saved Slack setup record.
    method, headers: {Origin: base, 'X-CSRF-Token': csrf}, ...(body === undefined ? {} : {data: body}),
    timeout: 30_000,
  });
  assert(response.ok(), `Actual local API refused ${method} ${resource}: ${response.status()}`);
  return response.json();
}
async function operation(name, body = {}) {
  return api(`endpoints/${endpoint.id}/operations/${name}`, {
    ...body, _observatory_endpoint_revision: endpoint.revision,
  });
}
async function click(locator) { await openGuard(); await locator.click(); }
async function privateMacJson(file, directory = false) {
  const info = await fs.lstat(file);
  assert(!info.isSymbolicLink() && (directory ? info.isDirectory() : info.isFile()));
  assert.equal(info.uid, process.getuid()); assert.equal(info.mode & 0o077, 0);
  if (directory) return;
  assert(info.size <= 1024 * 1024);
  const bytes = await fs.readFile(file); assert(bytes.length <= 1024 * 1024);
  return JSON.parse(bytes);
}
async function macSetupUrl() {
  const control = path.join(input.root, '.observatory-launch-' + sha(input.home).slice(0, 16));
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    await openGuard();
    if (child.exitCode !== null || child.signalCode !== null) throw Error('LaunchServices exited before setup was observed');
    try {
      await privateMacJson(control, true);
      const session = await privateMacJson(path.join(control, 'setup-session.json'));
      const url = new URL(session.origin);
      assert.equal(url.protocol, 'http:'); assert.equal(url.hostname, '127.0.0.1');
      assert(url.port && url.pathname === '/' && !url.username && !url.password && !url.hash && !url.search);
      assert(typeof session.token === 'string' && /^[A-Za-z0-9_-]{20,256}$/.test(session.token));
      assert(Number.isSafeInteger(session.expires_at) && session.expires_at > Date.now()
        && session.expires_at <= Date.now() + 31 * 60_000);
      base = url.origin; return base + '/#' + session.token;
    } catch (error) {
      // Read-only polling may see the owner's initial create/write in progress.
      if (error.code !== 'ENOENT' && !(error instanceof SyntaxError)) throw error;
    }
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  throw Error('LaunchServices private setup readiness deadline');
}
async function macControl(stop = false) {
  assert(macLaunch && macWitness);
  await verifyFixtureObserver();
  const observer = spawn(input.fixture_python, [input.fixture_controller,
    stop ? '--mac-stop-desktop' : '--mac-observe-desktop', process.argv[2], '--mac-witness', macWitness],
  {env: input.application_env, stdio: ['ignore', 'pipe', 'pipe']});
  let value = '', diagnostic = '', timer;
  observer.stdout.on('data', data => { if (value.length <= 1024 * 1024) value += data; });
  observer.stderr.on('data', data => { if (diagnostic.length <= 1024 * 1024) diagnostic += data; });
  try {
    const exit = await Promise.race([
      new Promise((resolve, reject) => { observer.once('error', reject); observer.once('close', (code, signal) => resolve({code, signal})); }),
      new Promise((_, reject) => { timer = setTimeout(() => reject(Error('Exact desktop observation/stop is unknown')), stop ? 15_000 : 10_000); }),
    ]);
    assert.equal(exit.code, 0, 'Exact desktop ownership/stop was not confirmed'); assert.equal(exit.signal, null);
    assert(value.length <= 1024 * 1024 && diagnostic.length <= 1024 * 1024);
    return JSON.parse(value);
  } finally {
    clearTimeout(timer);
    await fs.writeFile(output(`mac-desktop-${launchNumber}-${stop ? 'stop' : 'observe'}.stderr`), diagnostic);
    // No kill or repeated stop after a transport timeout. The exact helper's
    // saved intent/result remains available in the retained private root.
    observer.stdout.destroy(); observer.stderr.destroy(); observer.unref();
  }
}
async function bindMacPayload() {
  if (!macLaunch) return;
  macPayload = await macControl();
  if (macGenerations.length) assert.notDeepEqual(macPayload.process_identity, macGenerations.at(-1));
  macGenerations.push(macPayload.process_identity);
  await checkpoint('launchservices-service-generation-bound', {launch: launchNumber,
    opener_pid: child.pid, process_identity: macPayload.process_identity, proof: macPayload.proof});
}
async function start(first) {
  await openGuard();
  if (macLaunch) {
    assert.equal(++launchNumber, first ? 1 : 2); macPayload = undefined;
    macWitness = path.join(input.root, `mac-desktop-${launchNumber}.json`);
    const files = ['stdout', 'stderr'].map(kind => path.join(input.root, `mac-desktop-${launchNumber}.${kind}`));
    for (const file of files) { const handle = await fs.open(file, 'wx', 0o600); await handle.close(); }
    macLogs.push(files);
    assert.equal(sha(await fs.readFile(input.gui)), input.gui_sha256);
    child = spawn('/usr/bin/open', ['-n', '-W', '--stdout', files[0], '--stderr', files[1], input.app,
      '--args', '--home', input.home, '--no-browser'], {env: input.application_env, stdio: ['ignore','pipe','pipe']});
    child.on('error', () => {}); // readiness remains unknown; never a fallback launch
    child.stdout.on('data', bytes => { stdout += bytes; }); child.stderr.on('data', bytes => { stderr += bytes; });
    await checkpoint(first ? 'launchservices-dispatched' : 'launchservices-reopened', {opener_pid: child.pid,
      application_identity: 'not yet observed', launch: launchNumber});
    return first ? macSetupUrl() : undefined;
  }
  child = spawn(input.binary, ['--home', input.home, '--no-browser'], {
    // Exercise the packaged GUI executable's normal Windows visibility.
    env: input.application_env, stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout.on('data', bytes => { stdout += bytes.toString(); });
  child.stderr.on('data', bytes => { stderr += bytes.toString(); });
  await checkpoint(first ? 'observatory-launched' : 'observatory-reopened', {pid: child.pid});
  if (!first) return;
  const lines = createInterface({input: child.stdout});
  let timer;
  try {
    const result = await Promise.race([
      once(lines, 'line').then(([line]) => JSON.parse(line)),
      once(child, 'exit').then(() => { throw Error('Desktop exited before readiness'); }),
      new Promise((_, reject) => { timer = setTimeout(() => reject(Error('Desktop readiness deadline')), 30_000); }),
    ]);
    assert.equal(result.state, 'setup');
    const url = new URL(result.url);
    assert.equal(url.protocol, 'http:'); assert.equal(url.hostname, '127.0.0.1');
    assert(url.hash.length > 10); base = url.origin;
    return result.url; // capability stays in memory and is never copied to evidence
  } finally { clearTimeout(timer); lines.close(); }
}
async function stopOwnedParent() {
  assert(child && child.exitCode === null && child.signalCode === null);
  assert(!stopDispatches.has(child), 'An unconfirmed parent stop is never dispatched again');
  if (macLaunch) {
    assert(macPayload, 'Unbound LaunchServices application is retained without a guessed stop');
    stopDispatches.add(child);
    const result = await macControl(true);
    assert.equal(result.same_generation_absent, true); assert.equal(result.stop_dispatches, 1);
    assert.deepEqual(result.process_identity, macPayload.process_identity);
    macStops.push(result);
    if (child.exitCode === null && child.signalCode === null) {
      let timer;
      try { await Promise.race([once(child, 'exit'), new Promise((_, reject) => {
        timer = setTimeout(() => reject(Error('LaunchServices waiter exit is unknown')), 10_000);
      })]); } finally { clearTimeout(timer); }
    }
    assert.equal(child.exitCode, 0); assert.equal(child.signalCode, null);
    parentExited = true;
    await checkpoint('observatory-parent-exited', {launch: launchNumber, process_identity: result.process_identity,
      same_generation_absent: true, opener_pid: child.pid, opener_exit_code: child.exitCode});
    return;
  }
  const process = child, exited = once(process, 'exit');
  stopDispatches.add(process);
  process.kill(); // exact child handle, never an executable-name or descendant kill
  let timer;
  try {
    await Promise.race([exited, new Promise((_, reject) => {
      timer = setTimeout(() => reject(Error('Owned desktop exit is unknown')), 10_000);
    })]);
  } finally { clearTimeout(timer); }
  assert(process.exitCode !== null || process.signalCode !== null);
  parentExited = true;
  await checkpoint('observatory-parent-exited', {pid: process.pid, code: process.exitCode, signal: process.signalCode});
}
async function slackRecord() {
  const database = new DatabaseSync(path.join(input.home, 'state.sqlite3'), {readOnly: true});
  let records;
  try { records = database.prepare('SELECT value FROM records WHERE kind=?').all('slack_setup').map(row => JSON.parse(row.value)); }
  finally { database.close(); }
  assert.equal(records.length, 1);
  const record = records[0];
  assert.equal(record.endpoint_id, endpoint.id);
  assert.equal(record.private.binding_initialized, true);
  assert.equal(record.private.lifecycle_verified, true);
  assert.equal(record.private.sha256, input.slack.files[input.slack.entry].sha256);
  assert.equal(sha(await fs.readFile(record.private.executable)), record.private.sha256);
  return record;
}
async function nativeStatus(record) {
  const value = await nativeCall(record, 'native-status');
  nativeBinding(value, record);
  assert.equal(value.state, 'ready'); assert.equal(value.request_current, true);
  assert.equal(value.profile_matches, true); assert.equal(value.launch_observed, true);
  assert.equal(value.startup_change_supported, false);
  assert.equal(value.request.request_id, record.private.native_start_request_id);
  assert(Number.isSafeInteger(value.pid) && value.pid > 0);
  assert.equal(value.process_identity.pid, value.pid);
  assert(Object.keys(value.process_identity).length >= 2);
  assert.deepEqual(value.process_identity, record.private.native_process_identity);
  return {id: value.id, home: value.home, executable: value.executable,
    service: value.service, request_id: value.request.request_id, process_identity: value.process_identity};
}
function nativeBinding(value, record) {
  assert.equal(value.schema, frozenSlackLifecycleContract);
  assert.equal(value.id, record.connector.id); assert.equal(value.home, record.private.home);
  assert.equal(value.executable, record.private.executable); assert.equal(value.autostart, false);
  assert.deepEqual(value.service, record.private.native_service);
}
async function nativeCall(record, command, request) {
  assert(['native-status','native-stop','native-retire'].includes(command));
  await openGuard();
  assert.equal(sha(await fs.readFile(record.private.executable)), record.private.sha256);
  const args = ['--home', record.private.home, command];
  if (request) args.push('--request-id', request);
  const process = spawn(record.private.executable, args, {
    env: input.application_env, stdio: ['ignore','pipe','pipe'], windowsHide: true,
  });
  let stdout = '', stderr = '', timer;
  const exited = new Promise((resolve, reject) => {
    process.once('error', reject);
    process.once('close', (code, signal) => resolve({code, signal}));
  });
  process.stdout.on('data', data => { stdout += data.toString('utf8'); });
  process.stderr.on('data', data => { stderr += data.toString('utf8'); });
  try {
    const exit = await Promise.race([exited, new Promise((_, reject) => {
      timer = setTimeout(() => reject(Error('Owned native manager response is unknown')), 100_000);
    })]);
    assert.equal(exit.code, 0, `Owned ${command} was not confirmed`);
    assert.equal(exit.signal, null);
    assert(Buffer.byteLength(stdout) <= 1024 * 1024 && Buffer.byteLength(stderr) <= 1024 * 1024);
    return JSON.parse(stdout);
  } catch (error) {
    // Unknown manager work is retained; this fixture neither kills it nor repeats
    // its command. Detach our observation streams so failure can be recorded.
    process.stdout.destroy(); process.stderr.destroy(); process.unref(); throw error;
  } finally { clearTimeout(timer); }
}
async function nativeControl(record, action) {
  assert(['stop','retire'].includes(action));
  const request = randomUUID();
  await checkpoint(`slack-${action}-intent`, {request_id: request,
    start_request_id: record.private.native_start_request_id, connector_id: record.connector.id});
  const value = await nativeCall(record, 'native-' + action, request);
  nativeBinding(value, record);
  assert.equal(value.control_current, true);
  assert.equal(value.state, action === 'stop' ? 'inactive' : 'retired');
  assert.equal(value.control.request_id, request); assert.equal(value.control.action, action);
  assert.equal(value.control.phase, value.state);
  assert.equal(value.control.start_request_id, record.private.native_start_request_id);
  assert.deepEqual(value.control.process_identity, record.private.native_process_identity);
  assert.equal(value.control.process_absent, true);
  assert.equal(value.control.observation, action === 'stop'
    ? 'exact_profile_inactive_and_service_lock_available' : 'exact_registration_absent');
  await checkpoint(`slack-${action}-confirmed`, {request_id: request, result: value});
  return value;
}
async function openAgent() {
  const close = page.getByRole('button', {name: 'Close popup', exact: true});
  if (await close.isVisible()) await click(close);
  await click(page.locator(`[data-action="agent"][data-agent="${endpoint.id}"]`).first());
}
async function openSavedSlack() {
  await openAgent();
  await click(page.getByRole('button', {name: 'Continue Slack setup', exact: true}));
  await page.locator('.ow-slack-setup').waitFor();
  await page.getByRole('button', {name: 'Connect to Slack', exact: true}).waitFor();
}

try {
  assert(await absent(input.home), 'Only an absent new desktop home is eligible');
  const url = await start(true);
  browser = await chromium.launch({headless: true, executablePath: input.browser});
  context = await browser.newContext({viewport: {width: 1200, height: 1000}, reducedMotion: 'reduce'});
  // Observe real HTTP calls. No route.fulfill, mocks, service workers, or remote consent.
  await context.route('**/*', async route => {
    const request = route.request(), url = new URL(request.url());
    if (url.origin !== base) { external.push({origin: url.origin, method: request.method()}); return route.abort(); }
    try { await openGuard(); } catch { return route.abort(); }
    let body;
    if (url.pathname.endsWith('/slack-setup') && request.method() === 'POST') body = request.postDataJSON();
    // Retain operation names/counts only. No passwords, API keys, capabilities,
    // cookies, control token, or full configuration in the result journal.
    requests.push({method: request.method(), path: url.pathname,
      ...(url.pathname.endsWith('/slack-setup') && body?.action ? {action: body.action} : {})});
    return route.continue();
  });
  page = await context.newPage(); page.setDefaultTimeout(15_000);
  page.on('pageerror', error => browserErrors.push(error.name));
  await page.goto(url);
  await page.getByLabel('Your name', {exact: true}).fill('Packaged consumer café 日本');
  await page.getByLabel('Choose an Observatory password', {exact: true}).fill(password);
  await page.getByLabel('Open Observatory when I sign in').uncheck();
  // Observe this one browser request; never issue a diagnostic create/retry.
  // Use the existing setup allowance across the response and ready UI together.
  const setupDeadline = Date.now() + 180_000;
  const createReply = page.waitForResponse(response => {
    const address = new URL(response.url());
    return address.origin === base && address.pathname === '/desktop/create'
      && response.request().method() === 'POST';
  }, {timeout: 180_000});
  createReply.catch(() => {}); // the awaited rejection below remains authoritative
  await click(page.getByRole('button', {name: 'Open my constellation', exact: true}));
  const created = await createReply;
  const reply = {status: created.status(), accepted: created.ok()};
  if (!created.ok()) {
    // Never retain cookies, request bodies, arbitrary errors or private paths.
    const size = Number(created.headers()['content-length']);
    const remaining = setupDeadline - Date.now();
    if (Number.isSafeInteger(size) && size > 0 && size <= 8192 && remaining > 0) {
      let timer;
      try {
        const bytes = await Promise.race([created.body(), new Promise((_, reject) => {
          timer = setTimeout(() => reject(Error('Original setup observation allowance elapsed')), remaining);
        })]);
        if (bytes.length === size) {
          reply.error_body_sha256 = sha(bytes);
          try {
            const value = JSON.parse(bytes.toString('utf8'));
            reply.error_class = value.error === 'Directory is not private current-user storage'
              ? 'private_directory_refused' : 'other_setup_error';
          } catch { reply.error_class = 'unreadable_setup_error'; }
        }
      } catch { reply.error_class = 'unavailable_setup_error'; }
      finally { clearTimeout(timer); }
    }
  }
  await checkpoint('desktop-create-response', reply);
  assert(created.ok(), 'Actual desktop setup refused; response status and partial home are retained');
  const setupRemaining = setupDeadline - Date.now();
  assert(setupRemaining > 0, 'Original desktop setup allowance elapsed');
  await page.locator('.ow-top').waitFor({timeout: setupRemaining});
  const session = await (await context.request.get(base + '/auth/session')).json();
  assert.equal(session.authenticated, true); csrf = session.csrf;
  await bindMacPayload();
  const accountSha = sha(await fs.readFile(path.join(input.home, 'secrets/local-password.json')));
  const desktopConfigSha = sha(await fs.readFile(path.join(input.home, 'config.json')));
  await checkpoint('actual-desktop-account-created');
  const host = (await api('hosts', undefined, 'GET')).hosts.find(value => value.native_local);
  assert(host && host.recipes.some(value => value.id === 'bundled-instance'));
  const previousDefault = (await api('approval-policy', undefined, 'GET')).new_agent_default;
  const selectedDefault = await api('approval-policy/default', {
    expected_revision: previousDefault.revision, mode: 'automatic',
  }, 'PUT');
  assert.equal(selectedDefault.mode, 'automatic');
  await checkpoint('fixture-owner-selected-canonical-new-agent-default', {mode: 'automatic', revision: selectedDefault.revision});

  await click(page.getByRole('button', {name: '+ Add', exact: true}));
  await click(page.locator('[data-action=create-agent]').filter({hasText: 'Create an agent'}));
  await page.getByLabel('Agent name', {exact: true}).fill('Disposable packaged agent');
  const planned = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/install/plan');
  await click(page.getByRole('button', {name: 'Review installation', exact: true}));
  const response = await planned; assert(response.ok());
  const request = response.request().postDataJSON();
  assert.match(request.request_id, /^[0-9a-f-]{36}$/);
  plan = {...await response.json(), request_id: request.request_id};
  assert.equal(path.basename(plan.home), plan.request_id);
  assert.equal(path.dirname(plan.home), host.install_roots[0]);
  assert.equal(plan.recipe_id, 'bundled-instance'); assert(await absent(plan.home));
  await fs.writeFile(output('owned-install-intent.json'), JSON.stringify({schema: 1,
    request_id: plan.request_id, home: plan.home, host_id: host.id, plan_digest: plan.plan_digest}, null, 2));
  await click(page.getByRole('button', {name: 'Install agent', exact: true}));
  await page.getByRole('button', {name: 'Continue setup', exact: true}).waitFor({timeout: 330_000});
  const job = await api('jobs/' + plan.request_id, undefined, 'GET');
  assert.equal(job.status, 'completed'); assert.equal(job.endpoint_id, plan.request_id);
  endpoint = (await api('endpoints', undefined, 'GET')).endpoints.find(value => value.id === job.endpoint_id);
  assert.equal(endpoint.management.provider, 'agent-instance');
  const policyDeadline = Date.now() + 20_000;
  let inheritedPolicy;
  do {
    inheritedPolicy = await operation('approval-policy');
    if (inheritedPolicy.mode === 'automatic') break;
    await new Promise(resolve => setTimeout(resolve, 200)); // read-only observation of existing inheritance
  } while (Date.now() < policyDeadline);
  assert.equal(inheritedPolicy.schema, 'agent-instance.approval-policy/1');
  assert.equal(inheritedPolicy.mode, 'automatic');
  assert.deepEqual(inheritedPolicy.rules, []); assert.deepEqual(inheritedPolicy.delegated_clients, []);
  const inheritedPolicySha = sha(Buffer.from(JSON.stringify(inheritedPolicy)));
  await checkpoint('actual-instance-canonical-default-inherited', {mode: inheritedPolicy.mode, revision: inheritedPolicy.revision,
    rules: inheritedPolicy.rules.length, delegated_clients: inheritedPolicy.delegated_clients.length});
  const installation = await privateJson(path.join(plan.home, 'install.json'));
  const instanceDatabase = new DatabaseSync(path.join(plan.home,'state.sqlite3'));
  try {
    instanceDatabase.exec('CREATE TABLE fixture_preserved_state (id TEXT PRIMARY KEY, value TEXT NOT NULL)');
    instanceDatabase.prepare('INSERT INTO fixture_preserved_state VALUES (?,?)').run('existing',retainedFixtureState);
  } finally {instanceDatabase.close();}
  const manifest = await privateJson(path.join(input.home, 'support/instance/consumer-bundle.json'));
  assert.equal(manifest.version, input.instance.version);
  assert.equal(sha(await fs.readFile(path.join(plan.home, 'bin', manifest.binary))), manifest.files[manifest.binary]);
  const runtime = await privateJson(path.join(plan.home, 'engine/installation.json'));
  assert.equal(runtime.sha256, manifest.native_runtime.binary_sha256); assert.equal(runtime.credentials_copied, false);
  await checkpoint('actual-bundled-instance-installed', {request_id: job.id, instance_id: endpoint.management.expected_id,
    home: plan.home, version: manifest.version, service: installation.service});
  await click(page.getByRole('button', {name: 'Continue setup', exact: true}));
  await page.locator('[data-setup=save]').waitFor();
  await page.waitForFunction(() => !document.querySelector('[data-setup=refresh]')?.disabled);
  beforeInstance = await operation('config'); beforeRuntime = await operation('native-config');
  assert.equal(beforeInstance.config.setup_complete, false); assert.equal(beforeInstance.config.enabled, false);
  // A write-only canary exercises local storage/readiness, never provider validation.
  assert.equal(beforeRuntime.config.provider.auth || 'auto', 'auto');
  await page.locator('[name=model]').fill('fixture-unqueried-model');
  await click(page.locator('[data-setup=save]'));
  await page.getByText('Model settings saved.', {exact: false}).waitFor();
  const intendedRuntime = structuredClone(beforeRuntime.config);
  intendedRuntime.provider.model = 'fixture-unqueried-model'; intendedRuntime.provider.auth = 'auto';
  delete intendedRuntime.provider.chatgpt_credentials_file;
  assert.deepEqual((await operation('native-config')).config, intendedRuntime);
  await page.locator('[data-setup-key]').fill(canary);
  await click(page.locator('[data-setup=key]'));
  await page.getByText('API key stored privately.', {exact: false}).waitFor();
  // Saving a key intentionally adds a private secret reference to Instance's
  // configuration. Verify that one change before checking the separate Start.
  const keyed = await operation('config');
  const keyEnvironment = intendedRuntime.provider.api_key_env;
  assert.match(keyEnvironment, /^[A-Za-z0-9_]{1,128}$/);
  const keyReference = keyed.config.backend.secret_env[keyEnvironment];
  assert.match(keyReference, /^native-model-key-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
  const keyedExpected = structuredClone(beforeInstance.config);
  keyedExpected.backend.secret_env[keyEnvironment] = keyReference;
  assert.deepEqual(keyed.config, keyedExpected);
  assert.equal(sha(await fs.readFile(path.join(plan.home, 'secrets', keyReference))),
    sha(Buffer.from(canary)), 'The disposable key must be stored at its exact private reference');
  await click(page.getByRole('button', {name: 'Start my agent', exact: true}));
  await page.getByRole('region', {name: 'Model account setup', exact: true})
    .locator('.ow-setup-feedback[role=status]')
    .filter({hasText: 'Your agent is configured and enabled for connections.'}).waitFor();
  const started = await operation('config'), status = await operation('status');
  assert.deepEqual(started.config, {...keyedExpected, setup_complete: true, enabled: true});
  assert.equal(status.instance_id, endpoint.management.expected_id);
  assert.equal(status.setup_complete, true); assert.equal(status.enabled, true);
  assert(Number.isSafeInteger(status.addresses.pid) && status.addresses.pid > 0);
  instanceCleanup = {home: plan.home, instance_id: status.instance_id, pid: status.addresses.pid,
    home_identity: await directoryIdentity(plan.home),
    install_sha256: sha(await fs.readFile(path.join(plan.home, 'install.json'))),
    binary_sha256: manifest.files[manifest.binary], binary: path.join(plan.home, 'bin', manifest.binary)};
  await checkpoint('actual-model-canary-and-explicit-start', {instance_id: status.instance_id,
    configured: true, enabled: true, provider_validated: false, model_requests: 0});
  await click(page.getByRole('button', {name: 'Continue to Slack', exact: true}));
  await click(page.locator('[data-action=slack-setup]'));
  await page.getByRole('button', {name: 'Set up Slack', exact: true}).waitFor();
  // Retain the existing request's response before closing the owned parent on
  // failure. A button timeout does not measure native setup duration. The same
  // 430-second allowance covers both this observation and the ready UI; no GET,
  // setup retry, private-record read or additional native command is introduced.
  const prepareStarted = Date.now(), prepareDeadline = prepareStarted + 430_000;
  const prepareReply = page.waitForResponse(response => {
    const address = new URL(response.url()), request = response.request();
    if (address.origin !== base || address.pathname !== `/api/v1/endpoints/${endpoint.id}/slack-setup`
        || request.method() !== 'POST') return false;
    try { return request.postDataJSON()?.action === 'prepare'; } catch { return false; }
  }, {timeout: 430_000});
  prepareReply.catch(() => {});
  await click(page.getByRole('button', {name: 'Set up Slack', exact: true}));
  let prepared;
  try { prepared = await prepareReply; }
  catch (error) {
    await checkpoint('slack-prepare-response-unavailable', {action: 'prepare',
      elapsed_ms: Date.now() - prepareStarted, response_observed: false});
    throw error;
  }
  const prepareObservation = {action: 'prepare', status: prepared.status(), accepted: prepared.ok(),
    elapsed_ms: Date.now() - prepareStarted, body_observation: 'unavailable'};
  const prepareSize = Number(prepared.headers()['content-length']);
  const bodyRemaining = prepareDeadline - Date.now();
  if (Number.isSafeInteger(prepareSize) && prepareSize > 0 && prepareSize <= 16_384 && bodyRemaining > 0) {
    let timer;
    try {
      const bytes = await Promise.race([prepared.body(), new Promise((_, reject) => {
        timer = setTimeout(() => reject(Error('Original Slack prepare observation allowance elapsed')), bodyRemaining);
      })]);
      if (bytes.length === prepareSize) {
        prepareObservation.projection = prepareResponseProjection(JSON.parse(bytes.toString('utf8')),
          prepared.request().postDataJSON(), endpoint);
        prepareObservation.body_observation = 'projected';
      } else { prepareObservation.body_observation = 'length_mismatch'; }
    } catch { prepareObservation.body_observation = 'unreadable_or_unavailable'; }
    finally { clearTimeout(timer); }
  }
  await checkpoint('slack-prepare-response', prepareObservation);
  assert(prepared.ok(), 'Actual Slack prepare HTTP reply refused; bounded response evidence is retained');
  const projection = prepareObservation.projection;
  assert(!projection?.refusal_code, 'Actual Slack prepare was refused before intent; response evidence is retained');
  if (projection?.operation_matches) assert(projection.uncertain !== true && projection.state !== 'unknown',
    'Actual Slack prepare outcome is unknown; saved response phase is retained without another request');
  const prepareRemaining = prepareDeadline - Date.now();
  assert(prepareRemaining > 0, 'Original Slack prepare observation allowance elapsed');
  await page.getByRole('button', {name: 'Connect to Slack', exact: true}).waitFor({timeout: prepareRemaining});
  const saved = await api(`endpoints/${endpoint.id}/slack-setup`, undefined, 'GET');
  assert.equal(saved.connector.binding_verified, true); assert.equal(saved.connector.endpoint_id, endpoint.id);
  assert.equal(saved.capabilities.can_authorize, true); assert.equal(saved.connector.setup_complete, false);
  let record = await slackRecord(); slackBefore = await nativeStatus(record);
  await checkpoint('actual-slack-native-setup-start-and-bind', {connector_id: saved.connector.id,
    operation_id: saved.operation.id, authorized_workspace: null, external_authorization: 'not attempted'});
  await stopOwnedParent();
  slackAfter = await nativeStatus(record); assert.deepEqual(slackAfter, slackBefore);
  await checkpoint('same-connector-generation-after-parent-exit');
  await start(false);
  const reopenedDeadline = Date.now() + 30_000;
  let listening = false;
  while (Date.now() < reopenedDeadline) {
    await openGuard();
    try { const reply = await context.request.get(base + '/auth/session', {timeout: 1000});
      if (reply.ok() && (await reply.json()).authenticated === true) { listening = true; break; } } catch {}
    await new Promise(resolve => setTimeout(resolve, 200)); // GET only; no repeated launch or setup
  }
  assert(listening, 'Reopened same desktop did not confirm its saved account');
  await bindMacPayload();
  await page.goto(base); await page.locator('.ow-top').waitFor({timeout: 30_000});
  await openSavedSlack();
  const reopened = await api(`endpoints/${endpoint.id}/slack-setup`, undefined, 'GET');
  assert.equal(reopened.operation.id, saved.operation.id);
  assert.equal(reopened.connector.id, saved.connector.id); assert.equal(reopened.connector.binding_verified, true);
  assert.equal(reopened.capabilities.can_authorize, true);
  const beforeSlackConfig = sha(await fs.readFile(path.join(record.private.home, 'adapter.json')));
  await nativeControl(record, 'stop');
  await click(page.getByRole('button', {name: 'Check saved setup', exact: true}));
  await page.getByRole('button', {name: 'Start saved connection', exact: true}).waitFor();
  await checkpoint('same-profile-explicit-resume-offered');
  await click(page.getByRole('button', {name: 'Start saved connection', exact: true}));
  await page.getByRole('button', {name: 'Connect to Slack', exact: true}).waitFor({timeout: 180_000});
  record = await slackRecord(); const resumed = await nativeStatus(record);
  assert.equal(resumed.id, slackBefore.id); assert.deepEqual(resumed.service, slackBefore.service);
  assert.notEqual(resumed.request_id, slackBefore.request_id);
  assert.notDeepEqual(resumed.process_identity, slackBefore.process_identity);
  assert.equal(sha(await fs.readFile(path.join(record.private.home, 'adapter.json'))), beforeSlackConfig);
  await checkpoint('same-saved-connector-explicitly-resumed', {before: slackBefore, after: resumed});
  assert.deepEqual((await operation('config')).config, started.config);
  assert.equal(sha(Buffer.from(JSON.stringify(await operation('approval-policy')))), inheritedPolicySha);
  assert.deepEqual((await api('approval-policy', undefined, 'GET')).new_agent_default, selectedDefault);
  assert.equal(sha(await fs.readFile(path.join(input.home, 'secrets/local-password.json'))), accountSha);
  assert.equal(sha(await fs.readFile(path.join(input.home, 'config.json'))), desktopConfigSha);
  const retainedDatabase = new DatabaseSync(path.join(plan.home,'state.sqlite3'),{readOnly:true});
  try {assert.equal(retainedDatabase.prepare('SELECT value FROM fixture_preserved_state WHERE id=?').get('existing').value,retainedFixtureState);}
  finally {retainedDatabase.close();}
  await checkpoint('fixture-owned-existing-state-sentinel-preserved', {actual_prior_conversation_proof:false});
  const storage = await page.evaluate(() => JSON.stringify({local: {...localStorage}, session: {...sessionStorage}}));
  for (const secret of [password, canary, csrf]) assert(!storage.includes(secret));
  assert.equal(requests.filter(r => r.path === '/api/v1/install' && r.method === 'POST').length, 1);
  assert.equal(requests.filter(r => r.path.endsWith('/slack-setup') && r.action === 'prepare').length, 1);
  assert.equal(requests.filter(r => r.path.endsWith('/slack-setup') && r.action === 'resume').length, 1);
  assert.equal(requests.filter(r => r.path.endsWith('/slack-setup') && ['authorize', 'confirm', 'connect'].includes(r.action)).length, 0);
  assert.equal(requests.filter(r => r.path.endsWith('/operations/lifecycle')).length, 1);
  assert.deepEqual(external, []); assert.deepEqual(browserErrors, []);
  await page.screenshot({path: output('actual-local-slack-ready.png'), fullPage: true});
  await checkpoint('same-saved-setup-reopened-without-reprovision');
  const stopped = await nativeControl(record, 'stop');
  const retired = await nativeControl(record, 'retire');
  assert.equal(sha(await fs.readFile(path.join(record.private.home, 'adapter.json'))), beforeSlackConfig);
  slackCleanup = {home: record.private.home, id: record.connector.id, stopped, retired,
    home_identity: await directoryIdentity(record.private.home), operation_id: record.operation.id,
    config_sha256: beforeSlackConfig, executable: record.private.executable, sha256: record.private.sha256,
    home_preserved: true};
  completed = true;
} finally {
  // A failure does not stop a newly installed service or erase a partial home.
  // Only close our browser and exact Observatory child; checked owned-profile
  // cleanup is a separate controller phase after a conclusive successful result.
  await browser?.close();
  if (child && child.exitCode === null && child.signalCode === null && !stopDispatches.has(child)
      && (!macLaunch || macPayload)) {
    try { await stopOwnedParent(); } catch { completed = false; }
  }
  if (macLaunch) {
    // Unbound/unknown LaunchServices generations and their waiter are retained.
    // Closing observer pipes is not a stop of the app or of its descendants.
    child?.stdout?.destroy(); child?.stderr?.destroy(); child?.unref();
    if (completed && (macGenerations.length !== 2 || macStops.length !== 2)) completed = false;
    try {
      for (const [out, err] of macLogs) {
        for (const [file, kind] of [[out, 'stdout'], [err, 'stderr']]) {
          const stat = await fs.lstat(file); assert(stat.isFile() && !stat.isSymbolicLink() && stat.size <= 4 * 1024 * 1024);
          const text = await fs.readFile(file, 'utf8');
          if (kind === 'stdout') stdout += text; else stderr += text;
        }
      }
      for (let launch = 1; launch <= launchNumber; launch++) {
        for (const suffix of ['.json', '.stop-intent.json', '.stopped.json']) {
          const name = `mac-desktop-${launch}${suffix}`, file = path.join(input.root, name);
          if (await absent(file)) continue;
          await privateMacJson(file);
          const bytes = await fs.readFile(file);
          await fs.writeFile(output(name), bytes, {flag:'wx', mode:0o600});
          macEvidence.push({file:name, size:bytes.length, sha256:sha(bytes)});
        }
      }
    } catch (error) {
      completed = false; await checkpoint('mac-evidence-retained-with-observation-error', {error_type:error.name});
    }
  }
  const safeText = value => { let out = value; for (const secret of [password, canary, csrf]) if (secret) out = out.split(secret).join('[fixture secret]');
    return out.replace(/(#)[A-Za-z0-9_-]{20,}/g, '#[setup capability]'); };
  await fs.writeFile(output('desktop.stdout'), safeText(stdout));
  await fs.writeFile(output('desktop.stderr'), safeText(stderr));
  const result = {schema: 1, status: completed ? 'local_setup_ready' : 'failed_or_unknown_retained',
    operation_id: input.operation_id, launch_mode: input.launch_mode, mac_service_generations: macGenerations,
    mac_service_stop_results: macStops, mac_evidence: macEvidence,
    observatory_home: input.home, instance_home: plan?.home,
    endpoint_id: endpoint?.id, instance_id: endpoint?.management?.expected_id,
    instance_cleanup: instanceCleanup, slack_cleanup: slackCleanup,
    parent_exited: parentExited, observations, requests, browser_errors: browserErrors,
    external_browser_requests: external, limits: input.limits, cleanup: 'controller required'};
  await fs.writeFile(output('browser-result.json'), JSON.stringify(result, null, 2));
}
