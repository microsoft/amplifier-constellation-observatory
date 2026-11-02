import sys,json
def send(v): print(json.dumps(dict(jsonrpc='2.0',**v)),flush=True)
def callback(i,method,params):
 send(dict(id=i,method=method,params=params)); return json.loads(sys.stdin.readline())
for line in sys.stdin:
 r=json.loads(line);m=r.get('method')
 if m=='initialize': result={'protocolVersion':1,'agentCapabilities':{'loadSession':True}}; assert r['params']['clientCapabilities']=={}
 elif m in ('session/new','session/load'): result={'sessionId':'session'}
 elif m=='session/prompt':
  send(dict(method='session/update',params={'sessionId':'session','update':{'sessionUpdate':'agent_message_chunk','content':{'text':'Preparing.'}}}))
  send(dict(method='session/update',params={'sessionId':'wrong','update':{'sessionUpdate':'agent_message_chunk','content':{'text':'FOREIGN'}}}))
  send(dict(method='session/update',params={'sessionId':'session','update':{'sessionUpdate':'agent_thought_chunk','content':{'text':'PRIVATE THOUGHT'}}}))
  send(dict(method='session/update',params={'sessionId':'session','update':{'sessionUpdate':'tool_call','toolCallId':'edit','title':'Write a note','status':'pending'}}))
  send(dict(method='session/update',params={'sessionId':'session','update':{'sessionUpdate':'plan','entries':[{'content':'Prepare note','status':'in_progress'}]}}))
  p={'sessionId':'wrong','toolCall':{'toolCallId':'edit','title':'Write a note','rawInput':{'path':'private-action.txt'}},'options':[{'optionId':'once','name':'Allow once','kind':'allow_once'}]}
  assert callback('wrong','session/request_permission',p)['result']['outcome']['outcome']=='cancelled'
  assert callback('fs','fs/read_text_file',{'sessionId':'session','path':'/tmp/a'})['error']['code']==-32601
  p['sessionId']='session'
  answer=callback('permission','session/request_permission',p)
  assert answer['result']['outcome']['optionId']=='once'
  send(dict(method='session/update',params={'sessionId':'session','update':{'sessionUpdate':'tool_call_update','toolCallId':'edit','status':'completed'}}))
  send(dict(method='session/update',params={'sessionId':'session','update':{'sessionUpdate':'agent_message_chunk','content':{'text':'Done.'}}}))
  result={'stopReason':'end_turn'}
 else: result={}
 send(dict(id=r['id'],result=result))
