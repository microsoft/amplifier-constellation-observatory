//! Real Windows kernel cleanup on both successful and failed ACP negotiation.
#![cfg(windows)]
use agent_observatory::{acp, config::Config, state::AppState};
use serde_json::{Value, json};
use std::path::PathBuf;
use windows_sys::Win32::{
    Foundation::{CloseHandle, WAIT_OBJECT_0},
    System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    },
};

#[tokio::test]
async fn closing_successful_or_failed_acp_client_stops_spawned_local_descendants() {
    for failure in [false, true] {
        let home = tempfile::tempdir().unwrap();
        Config {
            id: uuid::Uuid::new_v4().to_string(),
            owner: "fixture".into(),
            bind: "127.0.0.1".into(),
            port: 0,
            public_url: String::new(),
            auth_mode: Default::default(),
            web_transport: Default::default(),
            pam_service: "unused".into(),
            tls_cert: PathBuf::new(),
            tls_key: PathBuf::new(),
            tls_ca: None,
            embed_origins: vec![],
        }
        .save(home.path())
        .unwrap();
        let state = AppState::open(home.path().into()).unwrap();
        let output = home.path().join("fixture-pids.json");
        let script = r#"
import json, os, subprocess, sys
for line in sys.stdin:
    r=json.loads(line)
    if r.get('method') == 'initialize':
        child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        with open(sys.argv[1],'w') as out: json.dump([os.getpid(),child.pid],out)
        response={'jsonrpc':'2.0','id':r['id']}
        response.update({'error':{'code':-32603,'message':'fixture rejection'}} if sys.argv[2]=='fail' else {'result':{'protocolVersion':1,'agentCapabilities':{}}})
        print(json.dumps(response),flush=True)
"#;
        let result=acp::probe(&state,&json!({"command":"python","args":["-c",script,output,if failure {"fail"} else {"pass"}],"cwd":home.path()})).await;
        let pids: Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        let mut alive = vec![];
        for pid in pids.as_array().unwrap() {
            let pid = pid.as_u64().unwrap() as u32;
            // SAFETY: test-owned process IDs; valid handles are closed exactly once.
            unsafe {
                let process = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid);
                if !process.is_null() {
                    if WaitForSingleObject(process, 3000) != WAIT_OBJECT_0 {
                        alive.push(pid);
                        TerminateProcess(process, 1); // failed regression must not leak its fixture
                        WaitForSingleObject(process, 3000);
                    }
                    CloseHandle(process);
                } else {
                    assert_eq!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(87),
                        "Cannot inspect fixture process"
                    );
                }
            }
        }
        assert!(
            alive.is_empty(),
            "local ACP descendants survived client drop: {alive:?}"
        );
        assert_eq!(result.is_err(), failure);
    }
}
