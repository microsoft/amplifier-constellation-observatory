'use strict';
const $=id=>document.getElementById(id),key='observatory-desktop-setup';
const token=location.hash.slice(1)||sessionStorage.getItem(key);
if(location.hash){sessionStorage.setItem(key,token);history.replaceState(null,'',location.pathname)}
let request=null,busy=false;
async function api(path,body){
 const response=await fetch('/desktop/'+path,{method:body?'POST':'GET',headers:{Authorization:'Bearer '+token,...(body?{'Content-Type':'application/json'}:{})},body:body?JSON.stringify(body):undefined});
 const value=await response.json().catch(()=>({error:response.status===401?'Open the Observatory app again to continue setup.':'Setup could not be confirmed.'}));
 if(!response.ok){const error=new Error(value.error||'Setup could not be confirmed.');error.status=response.status;throw error}return value;
}
async function finished(recoverSignin=false){
 const status=await api('status');
 if(!status.ready)return false;
 // Same request restores only its sign-in reply; it cannot create an account or
 // enable startup twice. The password is never stored in browser storage.
 if(recoverSignin&&request)await api('create',{request_id:request,name:'',password:'',start_at_signin:false});
 $('password').value='';sessionStorage.removeItem(key);
 if(status.startup_warning){$('message').textContent=status.startup_warning;$('setup').hidden=true;$('check').hidden=false;$('check').textContent='Open Observatory';$('check').onclick=()=>location.replace('/');}
 else location.replace('/');
 return true;
}
if(!token){$('create').disabled=true;$('message').textContent='Open the Observatory app to begin setup.'}
$('setup').onsubmit=async event=>{
 event.preventDefault();if(busy||!token)return;busy=true;$('create').disabled=true;$('message').textContent='Getting your Observatory ready…';
 request ||= crypto.randomUUID();
 try{await api('create',{request_id:request,name:$('name').value,password:$('password').value,start_at_signin:$('startup').checked});await finished();}
 catch(error){$('message').textContent=error.message;if(error.status===400){request=null;$('create').disabled=false}else{$('check').hidden=false}}
 finally{busy=false}
};
$('check').onclick=async()=>{if(busy)return;busy=true;try{if(!await finished(true))$('message').textContent='Setup is not confirmed yet. Your existing files have been kept.'}catch(error){$('message').textContent=error.message}finally{busy=false}};
