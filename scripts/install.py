#!/usr/bin/env python3
"""Install one independent Observatory; no fleet discovery or implicit host grants."""
import argparse,json,os,pwd,shutil,subprocess,sys,time,socket
from pathlib import Path
os.umask(0o077)
p=argparse.ArgumentParser();p.add_argument('--home',type=Path,default=Path.home()/'.local/share/agent-observatory/default');p.add_argument('--binary',type=Path,required=True);p.add_argument('--owner',default=pwd.getpwuid(os.getuid()).pw_name);p.add_argument('--bind',default='0.0.0.0');p.add_argument('--port',type=int,default=0);p.add_argument('--public-url',default='');p.add_argument('--no-service',action='store_true');p.add_argument('--no-browser',action='store_true');a=p.parse_args()
home=a.home.expanduser().absolute()
if home.exists() and any(home.iterdir()):sys.exit('This installation folder is not empty; choose a new one.')
home.mkdir(parents=True,exist_ok=True,mode=0o700);(home/'bin').mkdir(mode=0o700);binary=home/'bin/agent-observatory';shutil.copy2(a.binary,binary);binary.chmod(0o700)
def run(args):return subprocess.run([str(x) for x in args],check=True,capture_output=True,text=True)
value=json.loads(run([binary,'--home',home,'init','--owner',a.owner,'--bind',a.bind,'--port',a.port,'--public-url',a.public_url]).stdout)
unit=f'agent-observatory-{value["id"]}.service'
if not a.no_service:
 pam=Path('/etc/pam.d/agent-observatory')
 if not pam.exists():
  profile=home/'pam-profile';profile.write_text('@include common-auth\n@include common-account\n')
  run(['sudo','install','-m','0644',profile,pam]);profile.unlink()
 def quote(v):return '"'+str(v).replace('\\','\\\\').replace('"','\\"').replace('%','%%')+'"'
 unit_text='\n'.join(['[Unit]','Description=Agent Observatory','After=network-online.target','','[Service]','Type=simple',f'ExecStart={quote(binary)} --home {quote(home)} serve','Restart=on-failure','RestartSec=5','UMask=0077','KillMode=control-group','','[Install]','WantedBy=default.target',''])
 unit_path=Path.home()/'.config/systemd/user'/unit;unit_path.parent.mkdir(parents=True,exist_ok=True);unit_path.write_text(unit_text)
 (home/'agent-observatory.service').write_text(unit_text)
 run(['systemctl','--user','daemon-reload']);run(['systemctl','--user','enable','--now',unit])
 for attempt in range(60):
  try:
   status=json.loads(run([binary,'--home',home,'status']).stdout)
   if status['id']!=value['id']:raise RuntimeError('Service identity did not match the installation')
   break
  except (subprocess.CalledProcessError,json.JSONDecodeError):time.sleep(.25)
 else:sys.exit('The service did not become healthy. Inspect '+unit+' before continuing.')
value.update(service=None if a.no_service else unit,binary=str(binary),certificate=str(home/'tls/root-ca.crt'))
value['local_url']=f'https://localhost:{value["port"]}'
try:
 addresses=sorted(set(socket.gethostbyname_ex(socket.gethostname())[2]))
 value['lan_urls']=[f'https://{address}:{value["port"]}' for address in addresses if not address.startswith('127.')]
except OSError:value['lan_urls']=[]
print(json.dumps(value,indent=2))
if not a.no_browser and not a.no_service and (os.environ.get('DISPLAY') or os.environ.get('WAYLAND_DISPLAY')) and shutil.which('xdg-open'):
 subprocess.Popen(['xdg-open',value['url']],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
