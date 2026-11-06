"""End-to-end OpenCode 2.0.25 native split on a disposable tmux server."""
import http.server,json,os,shlex,shutil,subprocess,sys,tempfile,threading,time
from pathlib import Path

repo=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(repo/'tests'))
from daemon_fixture import running_daemon
opencode = os.environ.get('WB_TEST_OPENCODE') or shutil.which('opencode')
assert opencode and subprocess.check_output([opencode, '--version'], text=True).strip() in ('2.0.25', 'opencode v2.0.25'), 'set WB_TEST_OPENCODE to the OpenCode 2.0.25 executable'

class Mock(http.server.BaseHTTPRequestHandler):
 seen=[]
 root_step=0
 child_step=0
 scripts=None
 def log_message(self,*args):pass
 def do_POST(self):
  body=json.loads(self.rfile.read(int(self.headers.get('content-length',0))))
  self.seen.append(body)
  tools=[x.get('function',{}).get('name') or x.get('name') for x in body.get('tools',[])]
  call=None
  messages=json.dumps(body.get('messages',[]))
  is_child='WB_CHILD_DIRECTION' in messages and 'please classify this isolated test task' not in messages
  if 'shell' in tools and 'subagent' in tools and not is_child:
   step=type(self).root_step;type(self).root_step+=1
   if step==0:call=('shell',{'command':'python3 '+shlex.quote(str(self.scripts/'place.py'))})
   elif step==1:call=('subagent',{'description':'split test direction','prompt':'WB_CHILD_DIRECTION: Split only the designated private tmux session by task meaning. Run the supplied isolated worker script.','agent':'general'})
   elif step==2:call=('shell',{'command':'python3 '+shlex.quote(str(self.scripts/'finish.py'))})
  elif 'shell' in tools and is_child:
   step=type(self).child_step;type(self).child_step+=1
   if step==0:call=('shell',{'command':'python3 '+shlex.quote(str(self.scripts/'worker.py'))})
  if call:
   tool,args=call
   part={'id':'m','object':'chat.completion.chunk','created':1,'model':'mock','choices':[{'index':0,'delta':{'tool_calls':[{'index':0,'id':'call_'+str(len(self.seen)),'type':'function','function':{'name':tool,'arguments':json.dumps(args)}}]},'finish_reason':None}]}
   end={'id':'m','object':'chat.completion.chunk','created':1,'model':'mock','choices':[{'index':0,'delta':{},'finish_reason':'tool_calls'}]}
   data=('data: '+json.dumps(part)+'\n\ndata: '+json.dumps(end)+'\n\ndata: [DONE]\n\n').encode()
   self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(data)));self.end_headers()
   try:self.wfile.write(data)
   except BrokenPipeError:pass
   return
  initial={'id':'m','object':'chat.completion.chunk','created':1,'model':'mock','choices':[{'index':0,'delta':{'role':'assistant','content':'mock complete'},'finish_reason':None}]}
  final={'id':'m','object':'chat.completion.chunk','created':1,'model':'mock','choices':[{'index':0,'delta':{},'finish_reason':'stop'}]}
  data=('data: '+json.dumps(initial)+'\n\ndata: '+json.dumps(final)+'\n\ndata: [DONE]\n\n').encode()
  self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(data)));self.end_headers()
  try:self.wfile.write(data)
  except BrokenPipeError:pass

server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Mock)
thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
try:
 with tempfile.TemporaryDirectory(prefix='wb-opencode-tui-') as temp:
  root=Path(temp);socket=str(root/'tmux.sock'); binary=str(repo/'target/debug/tmux-agent-workbench')
  Mock.scripts=root
  project=root/'project';project.mkdir();bindir=root/'bin';bindir.mkdir()
  (bindir/'tmux-agent-workbench').symlink_to(repo/'bin/tmux-agent-workbench-cli')
  (bindir/'opencode').symlink_to(opencode)
  team=bindir/'agent-team';team.write_text('#!/bin/sh\n[ "$1" = start ] && [ "$2" = opencode ] || exit 2\nshift 2\nexec opencode "$@"\n');team.chmod(0o755)
  config=root/'config';plugins=config/'opencode/plugins/tmux-agent-workbench-v2';plugins.mkdir(parents=True)
  wb=config/'tmux-agent-workbench';wb.mkdir();(wb/'config.toml').write_text('[notifications]\nenabled = false\n[auto_split]\nenabled = true\n')
  (project/'opencode.json').write_text(json.dumps({'autoupdate':False,'permissions':[{'action':'*','resource':'*','effect':'allow'}],'providers':{'wbmock':{'package':'aisdk:@ai-sdk/openai-compatible','name':'mock','settings':{'baseURL':f'http://127.0.0.1:{server.server_port}/v1','apiKey':'mock'},'models':{'mock':{'name':'Mock','limit':{'context':32000,'output':2048}}}}},'model':'wbmock/mock'}))
  env=dict(os.environ,TERM='xterm-256color',PATH=str(bindir)+':'+os.environ['PATH'],XDG_CONFIG_HOME=str(config),XDG_STATE_HOME=str(root/'state'),XDG_CACHE_HOME=str(root/'cache'),XDG_DATA_HOME=str(root/'data'),XDG_RUNTIME_DIR=str(root/'run'),TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket,OPENCODE_DISABLE_AUTOUPDATE='true',OPENCODE_DISABLE_MODELS_FETCH='true',NPM_CONFIG_REGISTRY='http://127.0.0.1:9',npm_config_registry='http://127.0.0.1:9',NPM_CONFIG_FETCH_RETRIES='0',npm_config_fetch_retries='0',NO_PROXY=(os.environ.get('NO_PROXY','')+',127.0.0.1,localhost').strip(','),no_proxy=(os.environ.get('no_proxy','')+',127.0.0.1,localhost').strip(','))
  for key in ('TMUX','TMUX_PANE','AGENT_TEAM_DIR','WORKBENCH_SESSION','WORKBENCH_TASK'):env.pop(key,None)
  def tmux(*args):return subprocess.check_output(['tmux','-u','-S',socket,*args],env=env,text=True,timeout=10).strip()
  def core(*args,data=None):return json.loads(subprocess.run([binary,*args],env=env,text=True,input=json.dumps(data) if data is not None else None,capture_output=True,timeout=10,check=True).stdout)
  common='''import json,os,subprocess\nfrom pathlib import Path\nr=Path(os.environ['WB_TEST_ROOT'])\ndef core(*args):return json.loads(subprocess.check_output(['tmux-agent-workbench',*args],text=True))\n'''
  (root/'place.py').write_text(common+'''pane=(r/'pane').read_text()\na=next(x for x in core('snapshot','--json')['agents'] if x['target']['pane_id']==pane and not x['exited'])\nresult=core('title','set','Picture layout','--session','Office-docs','--pane',pane,'--session-id',a['hook_session_id'])\n(r/'split.json').write_text(json.dumps(result))\nprint(json.dumps(result))\n''')
  (root/'worker.py').write_text(common+'''helper=os.environ['WB_HELPER']; result=json.loads((r/'split.json').read_text()); snap=json.loads(subprocess.check_output([helper,'snapshot'],text=True)); move=set(json.loads((r/'move.json').read_text())); groups=[]\nfor session in snap['sessions']:\n ids=[w['window_id'] for w in snap['windows'] if w['session_id']==session['session_id']]\n if session['session_id']==result['source_session']:\n  groups.append({'session':'Office-docs','windows':[w for w in ids if w not in move]});groups.append({'session':'Office-docs·layout','windows':[w for w in ids if w in move]})\n else:groups.append({'session':session['session_name'],'windows':ids})\n(r/'snapshot.json').write_text(json.dumps(snap));(r/'plan.json').write_text(json.dumps({'groups':groups}));subprocess.run([helper,'apply','--snapshot',str(r/'snapshot.json'),'--plan',str(r/'plan.json'),'--scope-session',result['source_session']],check=True);(r/'worker-done').write_text('native child completed')\n''')
  (root/'finish.py').write_text(common+'''result=json.loads((r/'split.json').read_text());assert (r/'worker-done').exists();core('auto-split','finish','--ticket',result['ticket'],'--result','completed');pane=(r/'pane').read_text();a=next(x for x in core('snapshot','--json')['agents'] if x['target']['pane_id']==pane and not x['exited']);out=core('title','set','Picture layout','--session','Office-docs·layout','--pane',pane,'--session-id',a['hook_session_id']);(r/'placed.json').write_text(json.dumps(out));print(json.dumps(out))\n''')
  env['WB_TEST_ROOT']=str(root);env['WB_HELPER']=str(repo/'bin/wb-reorganise')
  env['TMUX_ASSISTANT_RESURRECT_DIR']=str(root/'assistant-resurrect')
  env['TMUX_AGENT_WORKBENCH_BIN']=binary
  subprocess.run([binary,'hooks','install','opencode'],env=env,check=True,capture_output=True)
  try:
   fixture=str(repo/'target/debug/examples/codex');command='env WORKBENCH_FIXTURE_SECONDS=300 '+shlex.quote(fixture)
   existing=[tmux('-f','/dev/null','new-session','-d','-x','140','-y','42','-s','Office-docs','-n','codex','-P','-F','#{pane_id}',command)]
   for _ in range(4):existing.append(tmux('new-window','-d','-t','Office-docs:','-n','codex','-P','-F','#{pane_id}',command))
   env['TMUX']=socket+','+tmux('display-message','-p','#{pid}')+',0'
   with running_daemon(os.environ.get('WB_TEST_DAEMON', binary),env):
    deadline=time.monotonic()+20
    while time.monotonic()<deadline:
     agents=core('snapshot','--json')['agents'];live={a['target']['pane_id']:a for a in agents if not a['exited']}
     if all(p in live for p in existing):break
     time.sleep(.2)
    else:raise AssertionError('mock agents not detected')
    for i,p in enumerate(existing):
     sid='fixture-'+str(i);core('hook','ingest','codex','SessionStart',data={'session_id':sid,'agent_pid':live[p]['process']['pid']})
     core('title','set',['Text caret','Text styles','Comments','Pictures','Page layout'][i],'--session','Office-docs','--pane',p,'--session-id',sid)
    (root/'move.json').write_text(json.dumps([tmux('display-message','-p','-t',p,'#{window_id}') for p in existing[3:]]))
    command='cd '+shlex.quote(str(project))+' && exec '+shlex.quote(str(repo/'bin/wb-agent'))+' opencode'
    pane=tmux('new-session','-d','-s','launch','-n','opencode','-P','-F','#{pane_id}',command)
    (root/'pane').write_text(pane)
    start=time.monotonic(); agent=None
    while time.monotonic()-start<25:
     agent=next((a for a in core('snapshot','--json')['agents'] if a['target']['pane_id']==pane and not a['exited']),None)
     if agent:break
     time.sleep(.2)
    print('detected:',bool(agent),'kind:',agent and agent['kind'],'pid:',agent and agent['process']['pid'])
    time.sleep(6)
    screen=tmux('capture-pane','-p','-t',pane)
    print('screen before:',screen[-1600:])
    assert 'plugins failed' not in screen, 'legacy compatibility plugins failed to load'
    tmux('send-keys','-t',pane,'please classify this isolated test task')
    tmux('send-keys','-t',pane,'Enter')
    deadline=time.monotonic()+30
    while not (root/'placed.json').exists() and Mock.root_step < 4 and time.monotonic()<deadline:time.sleep(.2)
    time.sleep(1)
    print('screen after:',tmux('capture-pane','-p','-t',pane)[-1800:])
    agents=core('snapshot','--json')['agents'];a=next((a for a in agents if a['target']['pane_id']==pane and not a['exited']),None)
    print('hook bound:',bool(a and a.get('hook_session_id')),'requests:',len(Mock.seen))
    print('naming context:',any('The target capacity is' in json.dumps(body) for body in Mock.seen))
    print('root steps:',Mock.root_step,'native child steps:',Mock.child_step)
    if not (root/'worker-done').exists():
     for body in Mock.seen:
      for message in body.get('messages',[]):
       if message.get('role')=='tool':
        value=json.dumps(message.get('content',''),ensure_ascii=False)
        if any(x in value for x in ('Traceback','Error','error','denied')):print('tool failure:',value[-2500:])
    if (root/'split.json').exists():print('placement decision:',json.loads((root/'split.json').read_text()).get('status'))
    if (root/'placed.json').exists():print('final placement:',json.loads((root/'placed.json').read_text()).get('status'),tmux('display-message','-p','-t',pane,'#{session_name}'))
    assert (root/'placed.json').exists() and (root/'worker-done').exists(), 'native split did not complete'
    assert json.loads((root/'split.json').read_text())['status']=='needs_split'
    assert json.loads((root/'placed.json').read_text())['status']=='renamed'
    assert tmux('display-message','-p','-t',pane,'#{session_name}')=='Office-docs·layout'
    deadline=time.monotonic()+8
    while time.monotonic()<deadline:
     live=next((x for x in core('snapshot','--json')['agents'] if x['target']['pane_id']==pane and not x['exited']),None)
     if live and live['base_state']=='idle':break
     time.sleep(.2)
    else:raise AssertionError('OpenCode v2 did not report the completed turn as idle')
  finally:
   subprocess.run(['tmux','-S',socket,'kill-server'],env=env,capture_output=True,timeout=5)
finally:
 server.shutdown();server.server_close();thread.join(timeout=3)
