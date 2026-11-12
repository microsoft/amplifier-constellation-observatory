#!/usr/bin/env python3
"""Actual helper -> verified HTTPS -> actual Observatory, all disposable.

OBSERVATORY_BINARY selects an immutable server; MACHINE_HELPER_BINARY selects
this crate's executable. The TLS proxy drops one already-committed redeem reply.
No service startup, production state, model call or host provisioning is used.
"""
import hashlib,http.client,http.server,json,os,signal,socket,ssl,subprocess,tempfile,threading,time,urllib.request,urllib.error,uuid
from pathlib import Path
SERVER=Path(os.environ['OBSERVATORY_BINARY']).resolve()
HELPER=Path(os.environ.get('MACHINE_HELPER_BINARY',Path(__file__).resolve().parents[1]/'target/debug/constellation-machine-helper')).resolve()
checks=[]
def check(ok,text):
 if not ok:raise AssertionError(text)
 checks.append(text);print('PASS',text,flush=True)
def request(url,body=None,token=None,ctx=None,origin=None):
 headers={'Content-Type':'application/json'}
 if token:headers['Authorization']='Bearer '+token
 if origin:headers['Origin']=origin
 r=urllib.request.Request(url,data=None if body is None else json.dumps(body).encode(),headers=headers)
 try:
  with urllib.request.urlopen(r,context=ctx,timeout=25) as response:return response.status,json.load(response)
 except urllib.error.HTTPError as error:return error.code,json.load(error)
def wait(fn,seconds=25):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
  try:
   result=fn()
   if result:return result
  except (OSError,ValueError):pass
  time.sleep(.15)
 raise AssertionError('Timed out waiting for fixture condition')
with tempfile.TemporaryDirectory(prefix='machine-helper-actual-') as directory:
 root=Path(directory);serverhome=root/'observatory';helperhome=root/'helper';server=None;helper=None;proxy=None;threads=[];requests=[];drop={'redeem':True,'heartbeat_refuse':False,'revoke_refuse':False}
 try:
  subprocess.run([str(SERVER),'--home',str(serverhome),'init','--owner','fixture','--bind','127.0.0.1'],check=True,capture_output=True,text=True)
  config=json.loads((serverhome/'config.json').read_text());port=config['port'];token=(serverhome/'secrets/control-token').read_text().strip();ca=Path(config['tls_ca']);cert=Path(config['tls_cert']);key=Path(config['tls_key']);ctx=ssl.create_default_context(cafile=str(ca));actual=f'https://localhost:{port}'
  class Proxy(http.server.BaseHTTPRequestHandler):
   protocol_version='HTTP/1.1'
   def log_message(self,*args):pass
   def do_POST(self):
    raw=self.rfile.read(int(self.headers.get('Content-Length','0')))
    if self.path.endswith('/redeem'):requests.append(json.loads(raw))
    if (self.path.endswith('/revoke') and drop['revoke_refuse']) or (self.path.endswith('/heartbeat') and drop['heartbeat_refuse']):
     body=b'{"error":"fixture proxy refused this request"}';self.send_response(403);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body);return

    conn=http.client.HTTPSConnection('localhost',port,context=ctx,timeout=25)
    try:
     conn.request('POST',self.path,raw,{'Content-Type':'application/json',**({'Authorization':self.headers['Authorization']} if self.headers.get('Authorization') else {})});response=conn.getresponse();body=response.read()
     if self.path.endswith('/redeem') and response.status==200 and drop['redeem']:
      drop['redeem']=False;self.close_connection=True;self.connection.shutdown(socket.SHUT_RDWR);self.connection.close();return
     self.send_response(response.status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
    finally:conn.close()
  proxy=http.server.ThreadingHTTPServer(('127.0.0.1',0),Proxy);proxyctx=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);proxyctx.load_cert_chain(cert,key);proxy.socket=proxyctx.wrap_socket(proxy.socket,server_side=True);base=f'https://localhost:{proxy.server_port}';config['public_url']=base;(serverhome/'config.json').write_text(json.dumps(config));t=threading.Thread(target=proxy.serve_forever,daemon=True);t.start();threads.append(t)
  server=subprocess.Popen([str(SERVER),'--home',str(serverhome),'serve'],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,text=True)
  wait(lambda:request(actual+'/api/v1/descriptor',token=token,ctx=ctx)[0]==200)
  status,created=request(actual+'/api/v1/machine-pairings',{'request_id':str(uuid.uuid4())},token,ctx);check(status==200,'actual server creates scoped short-lived pairing')
  pairing=created['pairing'];bootstrap={'schema':1,'observatory_url':base,'observatory_id':config['id'],'observatory_name':'Disposable constellation','pairing_id':pairing['id'],'expires_at':pairing['expires_at'],'ca_pem':ca.read_text()};bootstrapfile=root/'observatory-pairing.json';bootstrapfile.write_text(json.dumps(bootstrap))
  def launch():
   p=subprocess.Popen([str(HELPER),'--home',str(helperhome),'--bootstrap',str(bootstrapfile),'--no-browser','setup'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
   line=p.stdout.readline().strip();check(line.startswith('http://127.0.0.1:'),'actual helper binds loopback setup only');origin,localtoken=line.split('/#');return p,origin,localtoken
  helper,origin,localtoken=launch()
  def local(path,body=None):return request(origin+'/api/'+path,body,localtoken,origin=origin)
  before_secret=(helperhome/'credential').read_text();before_state=json.loads((helperhome/'state.json').read_text());check(len(before_secret)==64 and before_state['machine_id'],'machine credential and identity exist before first claim')
  if os.environ.get('MACHINE_HELPER_BROWSER')=='1':
   env={**os.environ,'MACHINE_HELPER_LOCAL_URL':origin+'/#'+localtoken,'MACHINE_HELPER_PAIR_CODE':pairing['code']}
   subprocess.run(['node',str(Path(__file__).with_name('pair-browser.mjs'))],env=env,check=True,timeout=45)
   status=400
  else:status,result=local('pair',{'code':pairing['code'],'startup':False})
  check(status==400 and local('status')[1]['pending'],'committed reply loss leaves original request pending, not paired by assumption')
  pending=json.loads((helperhome/'state.json').read_text())['pending'];check(pending['request_id']==requests[0]['request_id'],'pending request journal matches actual dispatched identity')
  helper.terminate();helper.wait(timeout=10);helper,origin,localtoken=launch();status,result=local('pair',{'code':'','startup':False});check(status==200,'fresh helper process recovers the original accepted claim')
  check(len(requests)==2 and requests[0]==requests[1],'lost reply retry sends identical original code, identity, request and credential')
  connected=wait(lambda:(lambda s:s if s['state']=='connected' else None)(local('status')[1]))
  status,inventory=request(actual+'/api/v1/hosts',token=token,ctx=ctx);check(status==200 and len(inventory['hosts'])==1,'pairing retry creates one machine record')
  host=inventory['hosts'][0];check(host['machine']['state']=='online' and host['transport']=='paired' and not host['recipes'],'real heartbeat proves observation-only machine online')
  check(connected['startup_enabled'] is False and not (helperhome/'bin').exists(),'fixture never installs a startup service or a live agent')
  check('machine_credential' not in json.dumps(connected) and before_secret not in json.dumps(connected),'browser status never exposes the machine credential')
  if os.environ.get('MACHINE_HELPER_BROWSER')=='1':
   env={**os.environ,'MACHINE_HELPER_LOCAL_URL':origin+'/#'+localtoken}
   env.pop('MACHINE_HELPER_PAIR_CODE',None)
   subprocess.run(['node',str(Path(__file__).with_name('pair-browser.mjs'))],env=env,check=True,timeout=45)

  altered=dict(bootstrap,observatory_url='https://other.example');check(local('bootstrap',altered)[0]==400,'existing helper refuses redirect to another Observatory origin')
  altered=dict(bootstrap,ca_pem='different trust root');check(local('bootstrap',altered)[0]==400,'existing helper refuses changed CA trust')
  drop['heartbeat_refuse']=True
  wait(lambda:local('status')[1]['state']=='access_refused',seconds=22)
  status,inventory=request(actual+'/api/v1/hosts',token=token,ctx=ctx);check(inventory['hosts'][0]['machine']['state']!='disconnected','heartbeat proxy403 stops automatic calls without claiming confirmed server revocation')
  helper.terminate();helper.wait(timeout=10);helper,origin,localtoken=launch();check(local('status')[1]['state']=='access_refused','restart preserves distinct access-refused state')
  drop['heartbeat_refuse']=False;drop['revoke_refuse']=True
  status,result=local('disconnect',{});check(status==400 and local('status')[1]['revoke_pending'] and local('status')[1]['state']=='disconnect_unknown','proxy403 cannot settle revocation or remove its pending evidence')
  helper.terminate();helper.wait(timeout=10);helper,origin,localtoken=launch();check(local('status')[1]['state']=='disconnect_unknown' and local('status')[1]['revoke_pending'],'restart preserves ambiguous revocation for explicit reconciliation')
  drop['revoke_refuse']=False
  status,result=local('disconnect',{});check(status==200,'local disconnect revokes actual server trust')
  status,inventory=request(actual+'/api/v1/hosts',token=token,ctx=ctx);check(inventory['hosts'][0]['machine']['state']=='disconnected','revocation retains the machine record with truthful disconnected state')
  helper.terminate();helper.wait(timeout=10);helper,origin,localtoken=launch();check(local('status')[1]['state']=='disconnected' and not local('status')[1]['pending'],'restart preserves revocation and does not automatically pair')
  check((helperhome/'credential').read_text()==before_secret and json.loads((helperhome/'state.json').read_text())['machine_id']==before_state['machine_id'],'identity and private evidence survive pairing, restart and revoke')
  status,forbidden=request(actual+'/api/v1/hosts',token=before_secret,ctx=ctx);check(status in (401,403),'machine credential cannot read administration routes')
  receipt={'checks':checks,'passed':len(checks),'server_sha256':hashlib.sha256(SERVER.read_bytes()).hexdigest(),'helper_sha256':hashlib.sha256(HELPER.read_bytes()).hexdigest(),'actual_tls':True,'actual_api_and_helper':True,'injected_fault':'one accepted redeem response dropped by test TLS proxy','service_startup':False,'models':0,'production_operations':False}
  output=os.environ.get('MACHINE_HELPER_RESULT');
  if output:Path(output).write_text(json.dumps(receipt,indent=2)+'\n')
  print(json.dumps({'passed':len(checks),'server_sha256':receipt['server_sha256'],'helper_sha256':receipt['helper_sha256']},indent=2))
 finally:
  for p in (helper,server):
   if p and p.poll() is None:p.terminate();p.wait(timeout=10)
  if proxy:proxy.shutdown();proxy.server_close()
