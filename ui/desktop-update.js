'use strict';
const key='observatory-local-update',token=location.hash.slice(1)||sessionStorage.getItem(key),$=id=>document.getElementById(id);
if(location.hash){sessionStorage.setItem(key,token);history.replaceState(null,'',location.pathname)}
let running=false;
async function api(path,method='GET'){
 const r=await fetch(path,{method,headers:{Authorization:'Bearer '+token}});
 const v=await r.json();if(!r.ok)throw Error(v.error||'The update could not be confirmed.');return v;
}
async function inspect(){
 try{
  const s=await api('/status');$('version').textContent='Version '+s.version;
  if(s.phase==='applied'||s.phase==='rolled_back'){
   $('message').textContent=s.phase==='applied'?'Observatory is updated.':'The previous Observatory was restored. '+(s.error||'');
   $('apply').hidden=true;$('open').hidden=false;running=false;
   $('open').onclick=async()=>{await api('/close','POST').catch(()=>{});sessionStorage.removeItem(key);location.replace(s.url)};
  }else if(s.working){
   running=true;$('apply').disabled=true;$('message').textContent='Updating Observatory…';setTimeout(inspect,1000);
  }else if(s.phase==='deferred'){
   running=false;$('message').textContent=s.error||'Let the current operation finish, then try again.';$('apply').disabled=false;
  }else if(s.phase!=='prepared'){
   running=false;$('message').textContent='The update did not finish. Continue the same operation to reopen Observatory safely.';
   $('apply').textContent='Continue recovery';$('apply').disabled=false;
  }else{
   running=false;$('apply').disabled=false;
  }
 }catch(e){$('message').textContent=e.message;if(running)setTimeout(inspect,1500)}
}
$('apply').onclick=async()=>{
 if(running)return;running=true;$('apply').disabled=true;
 try{await api('/apply','POST');setTimeout(inspect,500)}
 catch(e){$('message').textContent=e.message;inspect()}
};
if(token)inspect();else{$('apply').disabled=true;$('message').textContent='Open the downloaded Observatory app to continue.'}
