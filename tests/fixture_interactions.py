import sys,json
send=lambda v:print(json.dumps(dict(jsonrpc='2.0',**v)),flush=True)
authed=False
for line in sys.stdin:
 r=json.loads(line);m=r.get('method');p=r.get('params',{})
 if m=='initialize':
  assert p['clientCapabilities']['elicitation']=={'form':{},'url':{}}
  result={'protocolVersion':1,'authMethods':[{'id':'browser','name':'Browser login'}]}
 elif m=='session/new' and not authed:
  send({'id':r['id'],'error':{'code':-32000,'message':'Authentication required'}});continue
 elif m=='authenticate':
  assert p['methodId']=='browser'
  send({'id':'url','method':'elicitation/create','params':{'requestId':r['id'],'mode':'url','url':'https://example.test/authorize','elicitationId':'external','message':'Authorize this fixture'}})
  a=json.loads(sys.stdin.readline());assert a['result']=={'action':'accept'};authed=True;result={}
 elif m=='session/new':result={'sessionId':'s'}
 elif m=='session/prompt':
  send({'id':'wrong','method':'elicitation/create','params':{'sessionId':'other','mode':'form','message':'Do not show','requestedSchema':{'type':'object','properties':{}}}})
  assert json.loads(sys.stdin.readline())['error']['code']==-32602
  send({'id':'question','method':'elicitation/create','params':{'sessionId':'s','mode':'form','message':'Pick a direction','requestedSchema':{'type':'object','properties':{'direction':{'type':'string','enum':['small','large']}},'required':['direction']}}})
  assert json.loads(sys.stdin.readline())['result']=={'action':'accept','content':{'direction':'small'}}
  send({'method':'session/update','params':{'sessionId':'s','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'Authorized and answered'}}}});result={'stopReason':'end_turn'}
 else:result={}
 send({'id':r['id'],'result':result})
