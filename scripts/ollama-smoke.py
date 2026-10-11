#!/usr/bin/env python3
"""Private actual Ollama load/unload acceptance through native MyWhisper UI.

Build ui_preview first. The runner imports a SHA-256-pinned public GGUF into
a separate Ollama server, never the user's server/model directory. It does
not claim Ollama's registry CLI pull succeeds; that separate failure is
retained in the release-audit evidence.
"""
from pathlib import Path
import os,sys,socket,subprocess,time,urllib.request,json,re,hashlib
repo=Path(__file__).resolve().parent.parent
if sys.argv[1:] != ['--child']:
 os.execv(sys.executable,[sys.executable,str(repo/'scripts/disposable.py'),'MYWHISPER_AUDIT_ROOT','ollamalive',sys.executable,str(Path(__file__).resolve()),'--child'])
root=Path(os.environ['MYWHISPER_AUDIT_ROOT'])
if not (root.name.startswith('mywhisper-ollamalive-audit.') and (root/'.disposable').is_file() and root.stat().st_uid==os.geteuid() and root.stat().st_mode&0o777==0o700):raise RuntimeError('invalid disposable Ollama root')
model='synthetic-audit:latest'
with socket.socket() as selection:
 selection.bind(('127.0.0.1',0));port=selection.getsockname()[1]
env=os.environ.copy();env.pop('OLLAMA_API_KEY',None);env.update(OLLAMA_HOST=f'127.0.0.1:{port}',OLLAMA_AUTH='false',OLLAMA_NO_CLOUD='true',OLLAMA_DEBUG_LOG_REQUESTS='false',OLLAMA_MODELS=str(root/'models'),CUDA_VISIBLE_DEVICES='-1',OLLAMA_NUM_PARALLEL='1',OLLAMA_MAX_LOADED_MODELS='1',OLLAMA_CONTEXT_LENGTH='2048')
(root/'models').mkdir(mode=0o700)
url=f'http://127.0.0.1:{port}'
def get(path):
 with urllib.request.urlopen(url+path,timeout=5) as response:return json.load(response)
with (root/'server.log').open('wb') as log:
 server=subprocess.Popen(['ollama','serve'],env=env,stdout=log,stderr=log)
 try:
  deadline=time.monotonic()+15
  while True:
   if server.poll() is not None:raise RuntimeError('private Ollama server exited before readiness')
   try:version=get('/api/version')['version'];break
   except urllib.error.URLError:
    if time.monotonic()>=deadline:raise RuntimeError('private server readiness deadline exceeded')
    time.sleep(.05)
  print('Private actual Ollama server version:',version,flush=True)
  print('Registry CLI pull remains unresolved; importing authentic pinned public GGUF for the native API contract check.',flush=True)
  digest='f535f83ec568d040f88ddc04a199fa6da90923bbb41d4dcaed02caa924d6ef57'
  expected=270885952
  artifact=root/'synthetic-source.gguf';checksum=hashlib.sha256();received=0;started=time.monotonic()
  with urllib.request.urlopen('https://registry.ollama.ai/v2/library/smollm2/blobs/sha256:'+digest,timeout=10) as response,artifact.open('xb') as output:
   while chunk:=response.read1(1024*1024):
    received+=len(chunk)
    if received>expected or time.monotonic()-started>=60:raise RuntimeError('public GGUF download exceeded its size/deadline')
    checksum.update(chunk);output.write(chunk)
  if received!=expected or checksum.hexdigest()!=digest:raise RuntimeError('public GGUF authentication failed')
  print('Actual public GGUF authenticated:',digest,received,'bytes',flush=True)
  definition=root/'Modelfile';definition.write_text('FROM '+str(artifact)+'\n')
  with (root/'import.log').open('wb') as output:
   try:subprocess.run(['ollama','create',model,'-f',str(definition)],env=env,stdout=output,stderr=output,check=True,timeout=30)
   except Exception:
    data=(root/'import.log').read_text(errors='replace');data=re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]','',data)
    print('Private model import diagnostic:',repr(data[-1200:]),flush=True);raise
  entry=next(value for value in get('/api/tags')['models'] if value['name']==model)
  print('Actual imported private model:',model,entry['digest'],entry['size'],'bytes',flush=True)
  for unload,theme,language in [('0','light','en'),('1','dark','es')]:
   child_env=env.copy();child_env.update(MYWHISPER_OLLAMA_AUDIT_URL=url,MYWHISPER_OLLAMA_AUDIT_MODEL=model,MYWHISPER_OLLAMA_AUDIT_UNLOAD=unload)
   subprocess.run([sys.executable,str(repo/'scripts/disposable.py'),'MYWHISPER_AUDIT_ROOT','ollamaui','env','GDK_BACKEND=x11','GTK_A11Y=none','G_DEBUG=fatal-criticals','dbus-run-session','--','xvfb-run','-a','timeout','70',str(repo/'target/debug/examples/ui_preview'),'settings',theme,language,'Language Models','--ollama-live-smoke'],env=child_env,check=True,timeout=80)
   resident=get('/api/ps')['models']
   if unload=='0':
    if not any(value['name']==model for value in resident):raise RuntimeError('native load did not leave its model resident')
    print('Actual provider confirms native load: model resident.',flush=True)
   else:
    deadline=time.monotonic()+5
    while any(value['name']==model for value in resident) and time.monotonic()<deadline:
     time.sleep(.05);resident=get('/api/ps')['models']
    if any(value['name']==model for value in resident):raise RuntimeError('native unload retained its model')
    print('Actual provider confirms native unload: no owned model resident.',flush=True)
 finally:
  server.terminate()
  try:server.wait(timeout=10)
  except subprocess.TimeoutExpired:server.kill();server.wait(timeout=3)
print('Private actual Ollama load/unload, native completion and server cleanup passed; host instance/models unchanged.',flush=True)
