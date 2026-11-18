// Observatory presents the connector's setup. Its installation owns all effects.
const copy = value => JSON.parse(JSON.stringify(value));
const text = (value, limit=1024) => typeof value === 'string' ? value.slice(0, limit) : '';
const escape = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const uuid = value => typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
const team = value => typeof value === 'string' && /^[TE][A-Z0-9]{2,31}$/.test(value);
const pendingStates = new Set(['preparing','starting','pending','running','dispatching']);
const capabilityNames = ['prepare','bind','resume','claim','authorize','confirm','connect','discard'];
// Only the Observatory-owned pre-intent proof can release a rejected intention.
// Generic HTTP errors and connector errors never establish non-dispatch.
const refusalMessages = Object.freeze({
  endpoint_changed:'This agent connection changed before the step started. Reopen its setup before continuing.',
  setup_changed:'Saved setup changed before this step started. Review the current setup before continuing.',
  invalid_input:'The setup request was refused before it started. Review this step and try again explicitly.',
  package_unavailable:'Slack setup is not available for this computer yet. No setup action was started.',
  existing_home:'The proposed setup folder already exists and was preserved. No setup action was started.',
  not_prepared:'This connector has not been prepared. Review the current setup before continuing.',
  step_unavailable:'This step is no longer available. No setup action was started; review the current setup.'
});
function correlatedRefusal(value,body,selected) {
  return value?.schema===1 && value.kind==='slack_setup_refusal' && value.refused_before_intent===true
    && value.endpoint_id===selected.id && value.endpoint_revision===selected.revision
    && value.action===body.action && value.request_id===(body.request_id??null)
    && value.operation_id===(body.operation_id??null) && value.expected_revision===body.expected_revision
    && Object.hasOwn(refusalMessages,value.code);
}

function workspace(value) {
  if (!value || !team(value.team_id)) return null;
  return {team_id:value.team_id, name:text(value.name,120), domain:text(value.domain,120)};
}

// Keep a bounded public projection; transport objects may also contain private handoff data.
export function publicSlackSetup(value, endpoint) {
  if (!value || value.schema !== 1 || value.endpoint_id !== endpoint.id || value.endpoint_revision !== endpoint.revision || !Number.isSafeInteger(value.revision) || value.revision < 0) throw Error('The saved setup does not match this agent. Refresh the agent before continuing.');
  if (value.operation != null && !uuid(value.operation.id)) throw Error('The saved setup could not be identified. It has not been repeated.');
  const op=value.operation, c=value.connector||{}, s=value.setup||{};
  return {
    schema:1, endpoint_id:value.endpoint_id, endpoint_revision:value.endpoint_revision, revision:value.revision,
    operation:op?{id:op.id,request_id:uuid(op.request_id)?op.request_id:null,phase:text(op.phase,80),state:text(op.state,80),uncertain:op.uncertain===true}:null,
    connector:{id:text(c.id,200),label:text(c.label,200),endpoint_id:text(c.endpoint_id,200),binding_verified:c.binding_verified===true,account_ready:c.account_ready===true,connection_ready:c.connection_ready===true,connection_running:c.connection_running===true,setup_complete:c.setup_complete===true},
    setup:{authorization:text(s.authorization,40),authorization_pending:s.authorization_pending===true,intended_workspace:workspace(s.intended_workspace),authorized_workspace:workspace(s.authorized_workspace),workspace_mismatch:s.workspace_mismatch===true,next_action:text(s.next_action,40)},
    capabilities:Object.fromEntries(capabilityNames.map(name=>['can_'+name,value.capabilities?.['can_'+name]===true])),
    message:text(value.message)
  };
}

export function deriveSlackSetupState(observation,{loading=false,uncertain=false}={}) {
  const result={phase:'loading',can:{},done:false,poll:false};
  if (!observation) return {...result,phase:uncertain?'unknown':'loading'};
  const {operation:op,connector:c,setup:s,capabilities:cap}=observation;
  if (uncertain || op?.uncertain || ['unknown','requires_reconciliation'].includes(op?.state)) return {...result,phase:'unknown'};
  if (pendingStates.has(op?.state)) return {...result,phase:'working',poll:true};
  if (cap.can_resume && op?.state==='complete') return {...result,phase:'resume',can:{resume:!loading}};
  if (cap.can_bind && op?.state==='complete') return {...result,phase:'bind',can:{bind:!loading}};
  if (c.setup_complete && c.binding_verified && c.endpoint_id===observation.endpoint_id && !!c.id) return {...result,phase:'done',done:!loading};
  if (!op) return {...result,phase:'prepare',can:{prepare:!loading&&cap.can_prepare}};
  if (!c.account_ready && cap.can_claim) return {...result,phase:'claim',can:{claim:!loading}};
  if (s.next_action==='confirm' || s.authorization_pending) return {...result,phase:'confirm',can:{confirm:!loading&&cap.can_confirm}};
  if (s.workspace_mismatch) return {...result,phase:'authorize',can:{authorize:!loading&&cap.can_authorize}};
  if (s.next_action==='connect' && s.authorized_workspace) return {...result,phase:'workspace',can:{connect:!loading&&cap.can_connect,discard:!loading&&cap.can_discard}};
  if (s.next_action==='authorize' || ['missing','needs_attention'].includes(s.authorization)) return {...result,phase:'authorize',can:{authorize:!loading&&cap.can_authorize}};
  return {...result,phase:'unavailable'};
}

export function slackAuthorizationUrl(setup) {
  const approved=setup?.authorized_workspace, intended=setup?.intended_workspace;
  // A previous approval for another workspace must not send the user back to
  // that workspace while they are correcting it. The owner still checks the
  // exact workspace before app creation.
  if (setup?.workspace_mismatch && team(intended?.team_id)) return `https://app.slack.com/client/${encodeURIComponent(intended.team_id)}`;
  const domain=approved?.domain;
  if (domain && /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(domain)) return `https://${domain}.slack.com/messages/@slackbot`;
  const id=approved?.team_id||intended?.team_id;
  return team(id)?`https://app.slack.com/client/${encodeURIComponent(id)}`:'https://app.slack.com/';
}

export function createSlackSetupController({endpoint,api,storage,storageKey,randomUUID=()=>globalThis.crypto.randomUUID(),now=()=>Date.now(),onChange=()=>{}}) {
  const selected={id:endpoint.id,revision:endpoint.revision};
  const route=`endpoints/${encodeURIComponent(selected.id)}/slack-setup`;
  const key=storageKey||`observatory-slack-setup:${selected.id}:${selected.revision}`;
  let disposed=false, generation=0, observation=null, loading=false, uncertain=false, error='', notice='', pendingPrepare=null, unreadablePending=false, mutation=null, command=null;
  try {
    const stored=storage?.getItem(key);
    if (stored) {
      const saved=JSON.parse(stored);
      if (!uuid(saved.request_id) || saved.endpoint_revision!==selected.revision || Object.keys(saved).some(k=>!['request_id','endpoint_revision'].includes(k))) throw Error('invalid saved request');
      pendingPrepare=saved.request_id;
    }
  } catch {unreadablePending=true;uncertain=true;error='Saved setup could not be read. Check this installation before preparing another one.';}
  const snapshot=()=>({observation:observation&&copy(observation),loading,uncertain,error,notice,...deriveSlackSetupState(observation,{loading,uncertain})});
  const emit=()=>{if(!disposed)onChange(snapshot());};
  const live=epoch=>!disposed&&epoch===generation;
  function accept(value) {
    const next=publicSlackSetup(value,selected);
    if (observation && next.revision<observation.revision) throw Error('Saved setup is older than the state already observed. Check again before continuing.');
    if (observation?.operation && next.operation?.id!==observation.operation.id) throw Error('The setup operation changed. It has not been repeated.');
    if (unreadablePending) {observation=next;uncertain=true;return;}
    if (pendingPrepare) {
      if (next.operation?.request_id!==pendingPrepare && next.operation?.id!==pendingPrepare) {
        observation=next;uncertain=true;return;
      }
      storage?.removeItem(key);pendingPrepare=null;
    }
    if (mutation && (next.operation?.id!==mutation.operation_id || next.revision<=mutation.revision)) {
      observation=next;uncertain=true;return;
    }
    observation=next;uncertain=false;mutation=null;
  }
  function captureCommand(value) {
    command=null;
    if(uncertain||deriveSlackSetupState(observation).phase!=='confirm')return;
    const candidate=value.authorization_command;
    if(candidate&&/^\/slackauthticket [A-Za-z0-9_+/=.-]{1,1024}$/.test(candidate.command)&&Number.isFinite(candidate.expires_at)&&candidate.expires_at>now())command={command:candidate.command,expires_at:candidate.expires_at};
  }
  async function refresh() {
    if(disposed||loading)return;
    const epoch=generation;loading=true;error='';emit();
    try {const value=await api(route,'GET');if(live(epoch)){accept(value);captureCommand(value);}}
    catch(e){if(live(epoch)){uncertain=true;error=e.message||'Saved setup could not be checked.';}}
    finally{if(live(epoch)){loading=false;emit();}}
  }
  async function act(action,input={}) {
    if(disposed||loading)return;
    const allowed=deriveSlackSetupState(observation,{uncertain}).can[action];
    if(!allowed)throw Error('Check saved setup before continuing.');
    let body;
    if(action==='prepare') {
      if(pendingPrepare)throw Error('An earlier setup request has not been confirmed.');
      const request_id=randomUUID();
      if(!uuid(request_id))throw Error('A setup request could not be created.');
      if(!storage)throw Error('This browser cannot retain setup progress. Enable session storage before continuing.');
      storage.setItem(key,JSON.stringify({request_id,endpoint_revision:selected.revision}));
      pendingPrepare=request_id;
      body={action,request_id,endpoint_revision:selected.revision,expected_revision:observation.revision};
    } else {
      body={action,operation_id:observation.operation.id,endpoint_revision:selected.revision,expected_revision:observation.revision};
      if(action==='claim') {
        if(typeof input.password!=='string'||input.password.length<12||input.password.length>1024)throw Error('Choose a connector password with at least 12 characters.');
        body.password=input.password;
      }
      if(action==='confirm') {
        const challenge=typeof input.challenge==='string'?input.challenge.trim():'';
        if(!challenge||challenge.length>256||/[\p{Cc}]/u.test(challenge))throw Error('Enter the confirmation code shown by Slack.');
        body.challenge=challenge;
      }
      if(action==='connect') {
        const approved=observation.setup.authorized_workspace;
        if(!approved||input.team_id!==approved.team_id)throw Error('Confirm the workspace currently shown before creating the connection.');
        body.team_id=approved.team_id;
      }
      mutation={operation_id:body.operation_id,revision:body.expected_revision};
    }
    const epoch=generation;loading=true;error='';notice='';emit();
    try {
      const value=await api(route,'POST',body);
      if(!live(epoch))return;
      if(correlatedRefusal(value,body,selected)) {
        if(action==='prepare') {
          if(pendingPrepare!==body.request_id)throw Error('The rejected setup intention changed.');
          storage.removeItem(key);pendingPrepare=null;
        } else {
          if(mutation?.operation_id!==body.operation_id || mutation?.revision!==body.expected_revision)throw Error('The rejected step intention changed.');
          mutation=null;
        }
        // The proof settles only this refusal. A new read supplies current state;
        // it never dispatches the rejected action or another action automatically.
        uncertain=true;command=null;
        const current=await api(route,'GET');
        if(!live(epoch))return;
        accept(current);captureCommand(current);error=refusalMessages[value.code];
      } else {
        accept(value);
        if(['confirm','discard','connect'].includes(action))command=null;
        captureCommand(value);
      }
    } catch {
      if(live(epoch)){uncertain=true;error='The last step was not confirmed. Check saved setup before continuing; it has not been repeated.';command=null;}
    } finally {
      delete body.password;delete body.challenge;
      if(live(epoch)){loading=false;emit();}
    }
  }
  return {snapshot,refresh,act,authorizationCommand(){if(command&&command.expires_at<=now())command=null;return command&&{...command};},dispose(){disposed=true;generation++;command=null;observation=null;onChange=()=>{};}};
}

export function mountSlackSetup(container,{endpoint,api,onBack=()=>{},onDone=()=>{}}) {
  let disposed=false,timer,controller;
  let storage;try{storage=globalThis.sessionStorage;}catch{}
  const button=(label,action,disabled=false,primary=false)=>`<button type="button" class="ow-button${primary?' ow-primary':''}" data-slack-setup="${action}" ${disabled?'disabled':''}>${escape(label)}</button>`;
  const label=w=>w?`${w.name||w.domain||'Slack workspace'} (${w.team_id})`:'the workspace you approve in Slack';
  function render(state) {
    if(disposed)return;
    clearTimeout(timer);
    const o=state.observation,s=o?.setup,c=o?.connector,command=controller.authorizationCommand();
    let content='';
    if(state.phase==='loading')content='<p role="status">Reading saved Slack setup…</p>';
    if(state.phase==='prepare')content=`<p>Set up a private Slack connection for ${escape(endpoint.label||'this agent')} on its computer. Existing connections stay unchanged.</p>${button('Set up Slack','prepare',!state.can.prepare,true)}`;
    if(state.phase==='resume')content=`<p>This saved Slack connector is stopped. Start the same installation to continue with its existing settings and conversations.</p>${button('Start saved connection','resume',!state.can.resume,true)}`;
    if(state.phase==='bind')content=`<p>The same Slack installation is now confirmed running. Finish linking it to this agent to continue setup.</p>${button('Finish connecting this agent','bind',!state.can.bind,true)}`;
    if(state.phase==='claim')content=`<form class="ow-slack-form" data-slack-form="claim"><label>Connector password<input type="password" name="password" autocomplete="new-password" minlength="12" maxlength="1024" required></label><p class="ow-slack-note">At least 12 characters. This belongs to this connector, separately from your Slack password.</p>${button('Save connector password','claim',!state.can.claim,true)}</form>`;
    if(state.phase==='authorize')content=`<p>This connection is for ${escape(label(s.intended_workspace))}.</p>${s.workspace_mismatch?`<p class="ow-warning">The saved approval belongs to ${escape(label(s.authorized_workspace))}. No app will be created in that workspace for this connection.</p>`:''}<p>Approve access in Slack. Its administrator may need to approve the app too. Observatory keeps your place while you do that.</p>${button('Connect to Slack','authorize',!state.can.authorize,true)}`;
    if(state.phase==='confirm')content=`<p>Approve this connection in ${escape(label(s.intended_workspace))}.</p><ol class="ow-slack-steps"><li>${command?`<p>Copy this private command.</p><output class="ow-slack-command" data-slack-command>${escape(command.command)}</output>${button('Copy command','copy')}`:'<p>The setup command is not stored in this page. If you already approved it in Slack, enter its confirmation code below. Otherwise check saved setup.</p>'}</li><li><a class="ow-button" href="${escape(slackAuthorizationUrl(s))}" target="_blank" rel="noopener noreferrer">Open Slack</a><p>Open Slackbot, paste the command, then send it. Approve access when Slack asks.</p></li><li><form class="ow-slack-form" data-slack-form="confirm"><label>Confirmation code from Slack<input name="challenge" autocomplete="off" spellcheck="false" maxlength="256" required></label>${button('Confirm Slack approval','confirm',!state.can.confirm,true)}</form></li></ol>`;
    if(state.phase==='workspace')content=`<p>Slack approved access to <strong>${escape(label(s.authorized_workspace))}</strong>.</p><p>Create ${escape(endpoint.label||'this agent')}’s Slack app in this workspace?</p>${button('Create connection in this workspace','connect',!state.can.connect,true)}${state.can.discard?button('Choose another workspace','discard'):''}`;
    if(state.phase==='working')content='<p role="status">Setup is continuing on the agent’s computer. You can leave this page and return to the same setup.</p>';
    if(state.phase==='unknown')content='<p class="ow-warning">Setup has not been confirmed. Check its saved state before continuing. No installation or Slack action will be sent again automatically.</p>';
    if(state.phase==='unavailable')content='<p>This installation has not offered a setup step. Check its saved state; existing connections remain unchanged.</p>';
    if(state.phase==='done')content=`<p><strong>${escape(c.label||'Slack connection')}</strong> is configured for this agent.</p><p>${c.connection_running?'The connector reports that it is running. Invite its app into the Slack channels where you want to use it.':'The connection is saved and paused. Its existing start setting has been preserved.'}</p>${s.authorized_workspace?`<a class="ow-button ow-primary" href="https://app.slack.com/client/${encodeURIComponent(s.authorized_workspace.team_id)}" target="_blank" rel="noopener noreferrer">Open this Slack workspace</a>`:''}`;
    container.innerHTML=`<section class="ow-slack-setup" aria-label="Slack connection setup"><h3>Connect your agent to Slack</h3>${content}${o?.message?`<p role="status">${escape(o.message)}</p>`:''}${state.error?`<p class="ow-error" role="alert">${escape(state.error)}</p>`:''}<p class="ow-slack-feedback" role="status"></p><div class="ow-slack-actions">${button('Back','back')}${button('Check saved setup','refresh',state.loading)}${state.done?button('Done','done',false,true):''}</div></section>`;
    if(state.poll&&!state.loading)timer=setTimeout(()=>controller.refresh(),2500);
  }
  controller=createSlackSetupController({endpoint,api,storage,onChange:render});
  async function action(name) {
    if(disposed)return;
    if(name==='back'){onBack();return;}
    if(name==='done'){if(controller.snapshot().done)onDone({connector_id:controller.snapshot().observation.connector.id});return;}
    if(name==='refresh'){await controller.refresh();return;}
    if(name==='copy') {
      const command=controller.authorizationCommand();
      try{if(!command)throw Error();await navigator.clipboard.writeText(command.command);if(!disposed)container.querySelector('.ow-slack-feedback').textContent='Command copied. Paste it into Slackbot and send it.';}
      catch{if(!disposed)container.querySelector('.ow-slack-feedback').textContent='Select and copy the displayed command, then paste it into Slackbot.';}
      return;
    }
    const form=container.querySelector(`[data-slack-form="${name}"]`),input={};
    if(form){for(const field of form.querySelectorAll('input')){input[field.name]=field.value;field.value='';}}
    if(name==='connect')input.team_id=controller.snapshot().observation?.setup.authorized_workspace?.team_id;
    try{await controller.act(name,input);}catch(e){if(!disposed)container.querySelector('.ow-slack-feedback').textContent=e.message;}
    finally{delete input.password;delete input.challenge;}
  }
  function clicked(event){const target=event.target.closest('[data-slack-setup]');if(target&&container.contains(target)){event.preventDefault();event.stopPropagation();action(target.dataset.slackSetup);}}
  function submitted(event){if(event.target.matches('[data-slack-form]')){event.preventDefault();event.stopPropagation();action(event.target.dataset.slackForm);}}
  container.addEventListener('click',clicked);container.addEventListener('submit',submitted);
  render(controller.snapshot());controller.refresh();
  return {refresh:()=>controller.refresh(),dispose(){disposed=true;clearTimeout(timer);controller.dispose();container.removeEventListener('click',clicked);container.removeEventListener('submit',submitted);for(const input of container.querySelectorAll('input'))input.value='';const command=container.querySelector('[data-slack-command]');if(command)command.textContent='';}};
}
