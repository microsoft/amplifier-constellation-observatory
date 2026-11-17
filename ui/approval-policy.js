// Observatory coordinates settings; each installation remains the policy owner.
const modes = new Set(['ask', 'automatic']);
const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
export const modeLabel = mode => mode === 'automatic' ? 'Automatic' : mode === 'ask' ? 'Ask' : 'Not confirmed';
const running = operation => operation?.status === 'running';
const validId = id => typeof id === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id);

export function createApprovalPolicyController({api, storage, storageKey, onChange = () => {}, uuid = () => crypto.randomUUID()}) {
    const state = {snapshot: null, operation: null, selected: '', loading: false, applying: false, uncertain: false, forbidden: false, error: '', requestId: null};
    const changed = () => onChange(state);
    const readSaved = () => {
        try { const id = storage.getItem(storageKey); if (validId(id)) state.requestId = id; } catch {}
    };
    const remember = id => {
        if (state.requestId && state.requestId !== id) storage.setItem(storageKey + ':previous:' + state.requestId, state.requestId);
        storage.setItem(storageKey, id); state.requestId = id;
    };
    const forget = () => { try { storage.removeItem(storageKey); } catch {} state.requestId = null; };
    const acceptOperation = operation => {
        if (!operation || !validId(operation.id) || !Array.isArray(operation.targets)) throw Error('Invalid operation response');
        state.operation = operation;
        state.uncertain = false;
        remember(operation.id);
    };
    const failure = error => {
        state.forbidden = error?.status === 403;
        state.error = state.forbidden ? 'An Observatory administrator can manage approval settings. Sign in with an administrator account to continue.'
            : 'Approval settings could not be checked. Your last observed settings are shown.';
    };
    async function readOperation() {
        if (!state.requestId) return;
        try {
            acceptOperation(await api('approval-policy/operations/' + state.requestId));
            state.error = '';
        } catch (error) {
            state.uncertain = true;
            state.forbidden = error?.status === 403;
            state.error = state.forbidden ? 'Administrator access is required to check this change.'
                : 'This change has not been confirmed. Check its status before submitting another change; it will not be sent again automatically.';
        }
    }
    async function refresh() {
        if (state.loading || state.applying) return;
        state.loading = true; state.error = ''; state.forbidden = false; readSaved(); changed();
        try {
            const snapshot = await api('approval-policy');
            if (!Array.isArray(snapshot.targets) || !Array.isArray(snapshot.preview?.targets)) throw Error('Invalid preview');
            state.snapshot = snapshot;
            if (!state.selected && modes.has(snapshot.mode)) state.selected = snapshot.mode;
            // The server's current operation wins over an older completed change
            // saved by this browser. Preserve prior tracking when switching IDs.
            const latest = snapshot.last_operation;
            if (latest && state.requestId && latest.id !== state.requestId) {
                await readOperation();
                if (running(latest) || (!state.uncertain && !running(state.operation))) acceptOperation(latest);
            } else if (latest) acceptOperation(latest);
            if (state.requestId) await readOperation();
        } catch (error) { failure(error); }
        finally { state.loading = false; changed(); }
    }
    function select(mode) { if (modes.has(mode) && !state.applying && !running(state.operation) && !state.uncertain) { state.selected = mode; changed(); } }
    function canApply() {
        if (state.loading || state.applying || state.forbidden || state.uncertain || running(state.operation) || !modes.has(state.selected)) return false;
        const targets = state.snapshot?.targets || [];
        return targets.some(t => t.status === 'ready' && t.mode !== state.selected);
    }
    async function apply() {
        if (!canApply()) return;
        const request = {request_id: uuid(), mode: state.selected, targets: structuredClone(state.snapshot.preview.targets)};
        state.applying = true; state.error = ''; state.operation = null;
        try { remember(request.request_id); }
        catch { state.applying = false; state.error = 'This browser cannot save change tracking. Allow local storage before applying settings.'; changed(); return; }
        changed();
        try { acceptOperation(await api('approval-policy/operations', 'POST', request)); }
        catch (error) {
            // A definite rejected request can be freshly reviewed. A lost reply cannot be retried.
            if ([400, 403, 409, 422].includes(error?.status)) {
                forget(); state.forbidden = error.status === 403; state.snapshot = null;
                state.error = state.forbidden ? 'Administrator access is required. No change was submitted.'
                    : 'The agents or their settings changed. Refresh and review the current settings before applying again.';
            } else {
                state.uncertain = true;
                await readOperation();
            }
        } finally { state.applying = false; changed(); }
        if (state.operation && !running(state.operation) && !state.uncertain) await refresh();
    }
    async function poll() {
        if (state.applying || state.loading || !state.requestId) return;
        state.loading = true;
        try { await readOperation(); } finally { state.loading = false; changed(); }
        if (state.operation && !running(state.operation) && !state.uncertain) await refresh();
    }
    return {state, refresh, select, apply, poll, canApply, needsPoll: () => running(state.operation) || state.uncertain};
}

function targetStatus(target, operation) {
    const observed = target.observed;
    if (target.status === 'unknown') return observed?.mode ? `Change unconfirmed · currently ${modeLabel(observed.mode)}` : 'Change unconfirmed';
    if (['succeeded', 'unchanged'].includes(target.status)) return `${modeLabel(observed?.mode || operation?.mode || target.mode)} · confirmed`;
    if (target.status === 'ready') return modeLabel(target.mode);
    return ({pending:'Waiting to apply', sending:'Applying…', conflict:'Settings changed elsewhere', unsupported:'Not supported here', 'access-required':'Access needed', offline:'Not reachable', duplicate:'Same agent, another connection', 'not-sent':'Not changed'})[target.status] || 'Not confirmed';
}

export function renderApprovalPolicy(controller, {settingsLink = () => ''} = {}) {
    const s = controller.state, snapshot = s.snapshot;
    if (!snapshot) return `<div class="ow-approval-panel"><p role="status">${s.loading ? 'Checking approval settings for all your agents…' : s.error || 'Approval settings have not been checked.'}</p>${s.loading ? '<div class="ow-approval-loading" aria-hidden="true"></div>' : '<button type="button" class="ow-button" data-action="approvals-refresh">Check settings</button>'}</div>`;
    const targets = snapshot.targets, unique = targets.filter(t => t.status !== 'duplicate');
    const operation = s.operation, outcomes = new Map((operation?.targets || []).map(t => [t.endpoint_id, t]));
    const rows = targets;
    const confirmed = unique.filter(t => ['succeeded', 'unchanged'].includes(outcomes.get(t.endpoint_id)?.status)).length;
    const ready = unique.filter(t => t.status === 'ready').length;
    const blocked = s.loading || s.applying || s.forbidden || s.uncertain || running(operation);
    const summary = snapshot.mode === 'mixed' ? 'Your agents currently use different settings.'
        : modes.has(snapshot.mode) ? `Current setting: ${modeLabel(snapshot.mode)}${snapshot.complete ? ' for all agents.' : ' where confirmed.'}` : 'Some current settings could not be confirmed.';
    let result = '';
    if (operation) {
        const finished = !running(operation);
        result = `<section class="ow-approval-result" aria-live="polite"><h3>${finished ? `${confirmed} of ${unique.length} agents confirmed ${modeLabel(operation.mode)}` : 'Applying your choice…'}</h3><p>${finished ? operation.status === 'complete' ? 'The agents confirmed this change. Refresh to check their latest settings.' : 'Some agents still need attention. Their individual results are below. No failed or uncertain change is retried automatically.' : 'You can close this window. Observatory keeps the change record so you can check it later.'}</p></section>`;
    }
    return `<div class="ow-approval-panel"><p class="ow-approval-intro">Set how all ${unique.length} ${unique.length === 1 ? 'agent asks' : 'agents ask'} for tool approval across their chats.</p><p class="ow-approval-current">${esc(summary)}</p>
        ${unique.length ? `<fieldset class="ow-approval-choices" ${blocked ? 'disabled' : ''}><legend>When a tool needs approval</legend>
            <label><input type="radio" name="fleet-approval-mode" value="ask" data-control="approval-mode" data-key="approval-mode-ask" ${s.selected === 'ask' ? 'checked' : ''}><span><strong>Ask</strong><small>Ask for a decision, unless an “Always allow” rule already matches.</small></span></label>
            <label><input type="radio" name="fleet-approval-mode" value="automatic" data-control="approval-mode" data-key="approval-mode-automatic" ${s.selected === 'automatic' ? 'checked' : ''}><span><strong>Automatic</strong><small>Don’t ask for tool approvals. Automatically approve requests the agent offers.</small></span></label>
        </fieldset><p class="ow-approval-limits">Each agent keeps its own policy. Existing limits still apply. Sign-ins and questions are separate.</p>
        <div class="ow-dialog-actions"><button type="button" class="ow-button ow-primary" data-action="approvals-apply" data-key="approvals-apply" ${controller.canApply() ? '' : 'disabled'}>${s.applying ? 'Submitting…' : `Apply to all ${unique.length} ${unique.length === 1 ? 'agent' : 'agents'}`}</button><button type="button" class="ow-button" data-action="approvals-refresh" data-key="approvals-refresh" ${s.loading || s.applying ? 'disabled' : ''}>${running(operation) || s.uncertain ? 'Check change status' : 'Refresh settings'}</button></div>
        ${ready < unique.length ? `<p class="ow-approval-note">${unique.length - ready} ${unique.length - ready === 1 ? 'agent needs' : 'agents need'} attention. An unavailable agent keeps its current setting.</p>` : ''}` : '<p>No agents are connected to Observatory yet. Connect an agent to manage its approvals here.</p>'}
        ${s.error ? `<p class="ow-approval-error" role="alert">${esc(s.error)}</p>` : ''}${result}
        ${targets.length ? `<h3>All agents</h3><ul class="ow-approval-agents">${rows.map(t => `<li><div><strong>${esc(t.label || t.endpoint_id)}</strong><span class="ow-approval-state">${esc(targetStatus(t, null))}</span>${outcomes.has(t.endpoint_id) ? `<small>Last change: ${esc(targetStatus(outcomes.get(t.endpoint_id), operation))}</small>` : ''}${['offline','access-required','unsupported','conflict','unknown','not-sent'].includes(t.status) ? `<small>${esc(({offline:'Connect to this agent and refresh its settings.', 'access-required':'This connection needs permission to manage approvals.', unsupported:'This agent does not expose shared approval settings.', conflict:'Refresh to review its current settings.', unknown:'Check its settings before requesting another change.', 'not-sent':'No request was sent to this agent.'})[t.status])}</small>` : ''}</div>${settingsLink(t.endpoint_id)}</li>`).join('')}</ul>` : ''}
        <p class="ow-approval-note">“Always allow” rules stay with each agent. Open its settings, then Access &amp; credentials → Approvals, to review or revoke them. Switching to Ask keeps those rules.</p>
        <p class="ow-approval-note">This applies to the agents connected here now, including any outside the current view. Newly connected agents keep their own settings.</p></div>`;
}
