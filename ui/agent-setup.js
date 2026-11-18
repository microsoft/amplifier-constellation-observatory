// Installation-owned model setup. No prompts, background tests, secret drafts or backend replacement.
const escape = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const clone = value => JSON.parse(JSON.stringify(value));
const pending = job => ['starting','pending','cancelling'].includes(job?.state);
const mutationUncertain = error => !error.status || error.status >= 500 || error.status === 408;
const canonical = value => Array.isArray(value)?value.map(canonical):value&&typeof value==='object'?Object.fromEntries(Object.keys(value).sort().map(key=>[key,canonical(value[key])])):value;
const fingerprint = async value => Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',new TextEncoder().encode(JSON.stringify(canonical(value)))))).map(b=>b.toString(16).padStart(2,'0')).join('');

export function accountLink(job, native = true) {
  try {
    const url = new URL(native ? job.authorization_url : job.verification_url);
    if (url.protocol !== 'https:' || url.hostname !== 'auth.openai.com' || url.port || url.username || url.password || url.hash) return null;
    if (!native) return url.pathname === '/codex/device' && !url.search ? url.href : null;
    const callback = new URL(job.callback);
    if (url.pathname !== '/api/accounts/authorize' || callback.protocol !== 'http:' || callback.hostname !== '127.0.0.1' || Number(callback.port) < 1024 || callback.pathname !== '/auth/callback' || callback.search || callback.hash || callback.username || callback.password) return null;
    const redirects = url.searchParams.getAll('redirect_uri');
    return redirects.length === 1 && redirects[0] === callback.href ? url.href : null;
  } catch { return null; }
}

export function modelCandidate(snapshot, preset, model, auth, account) {
  if (!snapshot?.config?.provider || !model.trim()) throw Error('Choose a model before saving.');
  const config = clone(snapshot.config), current = config.provider;
  if (preset && preset.kind !== current.kind) {
    // Changing provider is explicit. The rest of the engine configuration remains intact.
    config.provider = {...clone(preset), model:model.trim()};
  } else config.provider.model = model.trim();
  config.provider.auth = auth;
  if (auth === 'chatgpt') {
    if (config.provider.kind !== 'openai' || account?.account?.connected !== true || !account.account.credentials_file) throw Error('Connect this agent’s ChatGPT account and choose OpenAI first.');
    config.provider.chatgpt_credentials_file = account.account.credentials_file;
  } else delete config.provider.chatgpt_credentials_file;
  return {expected_revision:snapshot.revision, expected_backend_sha256:snapshot.backend_sha256, config};
}

export function instanceSetupReady(observed, expectedId) {
  const document=observed?.document,status=observed?.status;
  return !!document && document.config?.instance_id===expectedId && status?.instance_id===expectedId
    && document.revision===status.config_revision && document.config.setup_complete===true
    && document.config.enabled===true && status.setup_complete===true && status.enabled===true;
}

export async function readInstanceSetup(call, expectedId) {
  const [document,status]=await Promise.all([call('config'),call('status')]);
  if(document?.config?.instance_id!==expectedId || status?.instance_id!==expectedId) throw Error('The agent identity changed. Its setup has not been changed.');
  if(!Number.isSafeInteger(document.revision)||document.revision<0||document.revision!==status.config_revision
    ||typeof document.config.setup_complete!=='boolean'||typeof document.config.enabled!=='boolean'
    ||document.config.setup_complete!==status.setup_complete||document.config.enabled!==status.enabled) throw Error('The agent changed while its setup was being read. Check saved setup before continuing.');
  return {document,status};
}

export function instanceFinishCandidate(document, expectedId) {
  if(document?.config?.instance_id!==expectedId||!Number.isSafeInteger(document.revision)) throw Error('The agent configuration could not be identified.');
  return {expected_revision:document.revision,config:{...clone(document.config),setup_complete:true}};
}

export async function instanceIntentSatisfied(intent, observed, endpointRevision, expectedId) {
  if(!['finish-instance-config','start-instance'].includes(intent?.action)||intent.endpoint_revision!==endpointRevision||intent.instance_id!==expectedId
    ||observed?.document?.config?.instance_id!==expectedId||observed?.status?.instance_id!==expectedId
    ||observed.document.revision!==observed.status.config_revision
    ||!Number.isSafeInteger(intent.previous_revision)||observed.document.revision<=intent.previous_revision
    ||!/^[a-f0-9]{64}$/.test(intent.config_sha256||''))return false;
  if(intent.action==='start-instance'&&!instanceSetupReady(observed,expectedId))return false;
  return observed.document.config.setup_complete===true && intent.config_sha256===await fingerprint(observed.document.config);
}

// One explicit Start. A lost write exits this function; status reads can later
// recognize its postcondition, but never continue the remaining mutation here.
export async function startAgentInstance({call,expectedId,endpointRevision,remember,confirmed,checkOpen=()=>{}}) {
  let observed=await readInstanceSetup(call,expectedId);checkOpen();
  if(instanceSetupReady(observed,expectedId))return observed;
  if(!observed.document.config.setup_complete) {
    const body=instanceFinishCandidate(observed.document,expectedId),config_sha256=await fingerprint(body.config);
    checkOpen();remember({action:'finish-instance-config',endpoint_revision:endpointRevision,instance_id:expectedId,previous_revision:body.expected_revision,config_sha256});
    await call('apply-config',body);checkOpen();
    observed=await readInstanceSetup(call,expectedId);checkOpen();
    if(observed.document.revision!==body.expected_revision+1 || await fingerprint(observed.document.config)!==config_sha256) throw Error('Setup completion was not confirmed against the intended settings. Start has not been sent.');
    checkOpen();confirmed();
  }
  if(instanceSetupReady(observed,expectedId))return observed;
  const before=observed.document,desired={...clone(before.config),enabled:true};
  const [prior_sha256,config_sha256]=await Promise.all([fingerprint(before.config),fingerprint(desired)]);checkOpen();
  // An explicit stop or another settings write between completion and Start
  // must not be crossed by this browser's already-pending continuation.
  const current=await readInstanceSetup(call,expectedId);checkOpen();
  if(current.document.revision!==before.revision||await fingerprint(current.document.config)!==prior_sha256) throw Error('The agent changed before Start. Its current state was kept; check saved setup.');
  checkOpen();remember({action:'start-instance',endpoint_revision:endpointRevision,instance_id:expectedId,previous_revision:before.revision,config_sha256});
  await call('lifecycle',{action:'start'});checkOpen();
  observed=await readInstanceSetup(call,expectedId);checkOpen();
  if(!instanceSetupReady(observed,expectedId)||observed.document.revision<=before.revision||await fingerprint(observed.document.config)!==config_sha256) throw Error('Start was not confirmed. Check saved setup before any further action.');
  checkOpen();confirmed();return observed;
}

export function mountAgentSetup(container, {endpoint, api, onDone=()=>{}, onBack=()=>{}, localBrowser=false}) {
  endpoint=clone(endpoint);
  let disposed=false, busy=false, timer, operations=new Set(), backend=null, native=null, schema=null, account=null, job=null, models=[], error='', message='', uncertain=false,instanceState=null;
  const savedKey=`observatory-agent-setup:${endpoint.id}:${endpoint.management?.expected_id}`;
  let unresolved=null;
  try { unresolved=JSON.parse(sessionStorage.getItem(savedKey)||'null');uncertain=!!unresolved; } catch { uncertain=true;error='Saved setup could not be read. No changes will be sent.'; }
  let providerChoice='', modelChoice='', authChoice='', dirty=false;
  const supported = id => operations.has(id);
  const checkOpen = () => { if(disposed)throw Error('Setup closed.'); };
  const call = async (id,body={}) => {
    checkOpen();
    if (!supported(id)) throw Error('This installation does not advertise that setup step.');
    const value=await api(`endpoints/${encodeURIComponent(endpoint.id)}/operations/${id}`,'POST',{...body,_observatory_endpoint_revision:endpoint.revision});
    checkOpen();return value;
  };
  const remember = value => {sessionStorage.setItem(savedKey,JSON.stringify(value));unresolved=value;};
  const confirmed = () => {sessionStorage.removeItem(savedKey);unresolved=null;uncertain=false;};
  const button = (label, action, disabled=false, primary=false) => `<button type="button" class="ow-button${primary?' ow-primary':''}" data-setup="${action}" ${disabled||busy?'disabled':''}>${escape(label)}</button>`;
  const presets = () => Array.isArray(schema?.['x-agent-runtime']?.provider_presets) ? schema['x-agent-runtime'].provider_presets.filter(p=>typeof p.kind==='string') : [];
  const isNative = () => backend?.kind === 'native';
  const accountReady = () => !uncertain && !dirty && !pending(job) && backend?.auth_ready === true && (!native || !!native.config?.provider?.model);
  const ready = () => accountReady() && instanceSetupReady(instanceState,endpoint.management?.expected_id);
  const canStart = () => accountReady() && !!instanceState && !instanceSetupReady(instanceState,endpoint.management?.expected_id)
    && supported('config')&&supported('status')&&supported('lifecycle')&&(instanceState.document.config.setup_complete||supported('apply-config'));
  function captureDraft() {
    const form=container.querySelector('[data-setup-form]');
    if (!form) return;
    providerChoice=form.elements.provider.value; modelChoice=form.elements.model.value; authChoice=form.elements.auth.value;
  }
  function schedule() {
    clearTimeout(timer);
    if (!disposed && pending(job)) timer=setTimeout(()=>refresh(false),2500);
  }
  function render() {
    if (disposed) return;
    const focused=container.contains(document.activeElement)?document.activeElement?.getAttribute('name'):null;
    const title=backend?.name || 'Model account', active=pending(job), connected=isNative()?account?.account?.connected===true:backend?.auth_ready===true;
    let content='',settings='';
    if (!backend) content='<p role="status">Reading this agent’s setup…</p>';
    else if (backend.kind === 'none' || backend.installed === false) content='<p>This agent’s model engine is not installed yet. Return to installation setup; no existing engine will be replaced here.</p>';
    else if (backend.kind === 'custom') content='<p>This agent uses its own model-account setup. Its selected engine and settings have been preserved.</p>';
    else {
      content=`<p class="ow-setup-summary"><strong>${escape(title)}</strong> · ${backend.auth_ready?'Account details are configured':'Model access needs setup'}</p><p class="ow-setup-note">Connect an account, review the model, then start your agent. Setup does not send a message or use model credits.</p>`;
      if (isNative() && native) {
        const options=presets(), current=native.config.provider;
        if (!options.some(p=>p.kind===current.kind)) options.unshift({kind:current.kind,label:current.kind});
        const fields=options.map(p=>`<option value="${escape(p.kind)}" ${p.kind===providerChoice?'selected':''}>${escape(p.label||p.name||({openai:'OpenAI',anthropic:'Anthropic',google:'Google'}[p.kind])||p.kind)}</option>`).join('');
        const catalog=schema?.['x-agent-runtime']?.model_catalog?.[providerChoice];
        const choices=authChoice==='chatgpt'?models:Array.isArray(catalog)?catalog.map(row=>({slug:row.model,display_name:row.label||row.display_name||row.model})):[];
        const retainedAuth=!['auto','chatgpt'].includes(authChoice)?`<option value="${escape(authChoice)}" selected>Keep current authentication</option>`:'';
        settings+=`<h3>Choose a model</h3><form class="ow-create-form ow-model-form" data-setup-form><label>Model provider<select name="provider">${fields}</select></label><label>Model<input name="model" value="${escape(modelChoice)}" list="ow-setup-models" autocomplete="off" required><datalist id="ow-setup-models">${choices.filter(m=>typeof m.slug==='string').map(m=>`<option value="${escape(m.slug)}">${escape(m.display_name||m.slug)}</option>`).join('')}</datalist></label><label>Account type<select name="auth">${retainedAuth}<option value="auto" ${authChoice==='auto'?'selected':''}>Provider API key</option>${supported('native-account')?`<option value="chatgpt" ${authChoice==='chatgpt'?'selected':''}>ChatGPT account</option>`:''}</select></label><p class="ow-setup-note">Selected settings stay unchanged until you save. Existing conversations keep their settings; changes apply to new connections.</p>${button('Save model settings','save',uncertain||active,true)}</form>`;
        if (native.config.provider.auth!=='chatgpt' && supported('native-credential')) settings+=`<section class="ow-setup-section"><h3>Provider API key</h3><p>For the saved ${escape(current.kind)} model settings. Stored privately for this agent and never displayed again.</p><label class="ow-setup-key">API key<input type="password" data-setup-key autocomplete="off" spellcheck="false" maxlength="65536" aria-describedby="ow-key-note"></label><p id="ow-key-note" class="ow-setup-note">Save model settings before adding a key for a different provider.</p>${button('Save API key','key',dirty||uncertain||active)}</section>`;
      }
      if (isNative()?supported('native-account'):supported('backend-login')) {
        content+=`<section class="ow-setup-section"><h3>ChatGPT account</h3><p>${connected?'An account is connected for this agent.':'Connect an account for this agent. Your provider settings will not change automatically.'}</p>`;
        if (active) {
          const link=accountLink(job,isNative()), canOpen=(!isNative()||localBrowser)&&job.state==='pending'&&link&&(!job.expires_at||job.expires_at>Date.now());
          content+=`<p role="status">${job.state==='starting'?'Preparing sign-in…':job.state==='cancelling'?'Cancelling sign-in…':'Waiting for your account authorization.'}</p>`;
          if (!isNative() && /^[A-Za-z0-9-]{1,64}$/.test(job.user_code||'')) content+=`<p class="ow-setup-code">${escape(job.user_code)}</p>`;
          if(canOpen) content+=`<a class="ow-button" href="${escape(link)}" target="_blank" rel="noopener noreferrer" data-setup-link>Open ChatGPT sign-in</a>`;
          else if(isNative()&&!localBrowser) content+='<p>Open Observatory on the agent’s own computer to finish this sign-in. Its private callback cannot be reached from another computer.</p>';
          else if(job.state==='pending') content+='<p>The returned sign-in address could not be verified. It has not been opened.</p>';
          content+=button('Cancel this sign-in','cancel',job.state==='cancelling'||(isNative()&&!job.id));
        } else {
          if (['failed','expired','interrupted','cancelled'].includes(job?.state)) content+=`<p>Sign-in ${escape(job.state)}. Check the current account before starting another attempt.</p>`;
          if(isNative()&&!localBrowser) content+='<p>ChatGPT sign-in needs Observatory open on this agent’s own computer. You can use a provider API key here instead.</p>';
          content+=button(connected?'Connect another account':'Connect ChatGPT','login',uncertain||(isNative()&&!localBrowser),!connected&&!backend.auth_ready);
          if(isNative()&&connected&&native.config.provider.kind==='openai'&&native.config.provider.auth!=='chatgpt')content+=button('Use my ChatGPT account','use-account',uncertain,true);
          if(isNative()&&connected&&supported('native-models')) content+=button('Show my account models','models',uncertain);
        }
        content+='</section>';
      }
      content+=settings;
    }
    if(instanceState)content+=`<section class="ow-setup-section"><h3>Start your agent</h3>${instanceSetupReady(instanceState,endpoint.management?.expected_id)?'<p>Your agent is configured and enabled for connections. No task or model request was sent.</p>':`<p>${instanceState.document.config.setup_complete?'Your agent is currently stopped. Its saved settings have been kept.':'Finish the saved setup and enable this agent for connections. Its other settings will stay unchanged.'}</p>${button('Start my agent','start',!canStart(),true)}`}</section>`;
    container.innerHTML=`<section class="ow-agent-setup" aria-label="Model account setup"><h3>Connect a model account</h3>${content}<p class="ow-setup-feedback" role="status">${escape(message)}</p>${error?`<p class="ow-error" role="alert">${escape(error)}</p>`:''}${uncertain?'<p>The last action was not confirmed. Check saved setup; it will not be sent again automatically.</p>':''}<div class="ow-setup-actions">${button('Back','back')}${button('Check saved setup','refresh')}${button('Continue to Slack','done',!ready(),true)}</div></section>`;
    if(busy)for(const field of container.querySelectorAll('input,select'))field.disabled=true;
    if(focused&&['provider','model','auth'].includes(focused))container.querySelector(`[name="${focused}"]`)?.focus({preventScroll:true});
  }
  async function refresh(reset=true) {
    if (disposed||busy) return;
    if (!reset) captureDraft();
    busy=true; error='';instanceState=null;render();
    try {
      const inspection=await api(`endpoints/${encodeURIComponent(endpoint.id)}/inspect`,'POST',{});
      checkOpen();
      operations=new Set((inspection.operations||[]).map(o=>o.id));
      if (inspection.instance_id!==endpoint.management?.expected_id) throw Error('The agent identity changed. Reconnect deliberately before setup.');
      if(supported('config')&&supported('status')) {
        instanceState=await readInstanceSetup(call,endpoint.management.expected_id);checkOpen();
        if(await instanceIntentSatisfied(unresolved,instanceState,endpoint.revision,endpoint.management.expected_id)) {
          checkOpen();const started=unresolved.action==='start-instance';confirmed();
          message=started?'The agent is now observed configured and enabled. The earlier reply was not received; Start was not repeated.':'Saved agent setup matches the requested settings. Start has not been sent; choose Start my agent when ready.';
        }
      }
      backend=await call('backend');
      if(isNative()) {
        [native,schema]=await Promise.all([call('native-config'),call('native-schema')]);
        // Instance's backend hash deliberately includes runtime.json, so a successful
        // save changes it. Reconcile desired content only, under the same pinned endpoint;
        // this is not a claim that the lost reply was delivered or the engine was tested.
        if(unresolved?.action==='save'&&unresolved.endpoint_revision===endpoint.revision&&unresolved.config_sha256===await fingerprint(native.config)) {
          checkOpen();confirmed();message='Saved settings now match the requested configuration. The earlier reply was not received; no write was repeated.';
        }
        if(supported('native-account')) {account=await call('native-account');job=account.login;}
        if(reset||!dirty) {providerChoice=native.config.provider.kind;modelChoice=native.config.provider.model;authChoice=native.config.provider.auth||'auto';dirty=false;}
      } else if(backend.kind==='codex'&&supported('backend-login')) job=await call('backend-login');
      // A status read may reveal completion; it never settles a lost mutation reply.
    } catch(e) {if(!disposed){error=e.message;instanceState=null;}}
    finally {busy=false;render();schedule();}
  }
  async function act(action) {
    if(disposed||busy) return;
    if(action==='back') {onBack();return;}
    if(action==='done') {if(ready())onDone({accountConfigured:true,instanceConfigured:true,instanceEnabled:true,modelTested:false});return;}
    if(action==='refresh') {await refresh(true);return;}
    captureDraft();busy=true;error='';message='';clearTimeout(timer);
    let mutation=false;
    // Extract and erase before any awaited work. Never place the secret in state or storage.
    const keyInput=container.querySelector('[data-setup-key]');
    let key=action==='key'?keyInput?.value:'';if(keyInput&&action==='key')keyInput.value='';
    render();
    try {
      if(uncertain) throw Error('An earlier action is still unconfirmed. Leave it unchanged and inspect the installation before another change.');
      if(action==='start') {
        if(!canStart())throw Error('Save the model account and settings, then check the agent before starting it.');
        instanceState=await startAgentInstance({call,expectedId:endpoint.management.expected_id,endpointRevision:endpoint.revision,
          remember:value=>{remember(value);mutation=true;},confirmed,checkOpen});
        message='Your agent is configured and enabled for connections. No task or model request was sent.';
      } else if(action==='login') {
        if(pending(job)) return;
        if(isNative()&&!localBrowser)throw Error('Open Observatory on the agent’s computer for ChatGPT sign-in.');
        remember({action,endpoint_revision:endpoint.revision,backend_sha256:native?.backend_sha256});mutation=true;
        job=await call(isNative()?'start-native-login':'start-backend-login',isNative()?{expected_backend_sha256:native.backend_sha256,callback_port:0}:{});
        confirmed();
      } else if(action==='cancel') {
        remember({action,endpoint_revision:endpoint.revision,job_id:job?.id||job?.login_id});mutation=true;job=await call(isNative()?'cancel-native-login':'cancel-backend-login',isNative()?{job_id:job.id}:{});confirmed();
      } else if(action==='use-account') {
        if(!isNative()||account?.account?.connected!==true||native.config.provider.kind!=='openai')throw Error('Connect this agent’s ChatGPT account first.');
        authChoice='chatgpt';dirty=true;
        if(supported('native-models')){const result=await call('native-models');models=result.models||[];}
        message='Choose your model below, then save to use this ChatGPT account. Your current settings have not changed.';
      } else if(action==='models') {
        const result=await call('native-models');models=result.models||[];
        message='Account models are available. Choose one above and save explicitly; your current model was not replaced.';
      } else if(action==='save') {
        const preset=presets().find(p=>p.kind===providerChoice);
        if(providerChoice!==native.config.provider.kind&&!preset) throw Error('This engine did not advertise that provider.');
        const body=modelCandidate(native,preset,modelChoice,authChoice,account);
        await call('validate-native-config',body);
        const config_sha256=await fingerprint(body.config);checkOpen();remember({action,endpoint_revision:endpoint.revision,backend_sha256:body.expected_backend_sha256,config_sha256});
        mutation=true;await call('apply-native-config',body);confirmed();
        dirty=false;message='Model settings saved. Existing conversations were not restarted. Model access has not been tested.';
        native=await call('native-config');backend=await call('backend');
      } else if(action==='key') {
        if(dirty)throw Error('Save your selected model settings before adding its key.');
        if(!key?.trim())throw Error('Enter an API key for the saved provider.');
        remember({action,endpoint_revision:endpoint.revision,backend_sha256:native.backend_sha256});mutation=true;await call('native-credential',{expected_backend_sha256:native.backend_sha256,value:key});key='';confirmed();
        native=await call('native-config');backend=await call('backend');
        message='API key stored privately. The first task you send will verify model access.';
      }
    } catch(e) {if(!disposed)error=e.message;if(action==='start')instanceState=null;if(mutation&&unresolved&&(action==='start'||mutationUncertain(e)))uncertain=true;else if(mutation)confirmed();}
    finally {key='';busy=false;render();schedule();}
  }
  function clicked(event) {const target=event.target.closest('[data-setup]');if(target&&container.contains(target)){event.preventDefault();event.stopPropagation();act(target.dataset.setup);}}
  function changed(event) {if(event.target.closest('[data-setup-form]')) {captureDraft();dirty=true;for(const action of ['key','done','start']){const control=container.querySelector(`[data-setup="${action}"]`);if(control)control.disabled=true;}}}
  function submitted(event) {if(event.target.matches('[data-setup-form]')) {event.preventDefault();act('save');}}
  container.addEventListener('click',clicked);container.addEventListener('input',changed);container.addEventListener('change',changed);container.addEventListener('submit',submitted);
  render();refresh();
  return {refresh:()=>refresh(true),dispose(){disposed=true;clearTimeout(timer);container.removeEventListener('click',clicked);container.removeEventListener('input',changed);container.removeEventListener('change',changed);container.removeEventListener('submit',submitted);const key=container.querySelector('[data-setup-key]');if(key)key.value='';account=null;job=null;}};
}
