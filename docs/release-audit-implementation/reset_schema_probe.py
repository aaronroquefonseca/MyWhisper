from pathlib import Path
import subprocess,tempfile,os,signal,shutil
repo=Path(__file__).resolve().parents[2];root=Path(tempfile.mkdtemp(prefix='mywhisper-reset-compat-')); (root/'.disposable').touch(mode=0o600)
env={**os.environ,'CARGO_TARGET_DIR':str(repo/'target'),'MYWHISPER_RESET_COMPAT_ROOT':str(root)}
changed=subprocess.check_output(['git','diff','--name-only'],text=True).splitlines()+['src/reset.rs','examples/ui_preview/reset_smoke.rs']
def snapshot(name,current):
 p=root/name;p.mkdir(); archive=subprocess.Popen(['git','archive', 'HEAD' if current else '21c7da7bb16255583574aa577a4d7e4e15050226'],stdout=subprocess.PIPE);subprocess.run(['tar','-x','-C',str(p)],stdin=archive.stdout,check=True);archive.stdout.close();assert archive.wait()==0
 if current:
  for file in changed:
   (p/file).parent.mkdir(parents=True,exist_ok=True);shutil.copy2(repo/file,p/file)
 # Different fixture package identities prevent Cargo from reusing a
 # baseline library when source mtimes from copied snapshots overlap.
 fixture_name='mywhisper-reset-compat-'+name
 f=p/'Cargo.toml'; body=f.read_text(); assert body.count('name = "mywhisper"') >= 1
 body=body.replace('name = "mywhisper"','name = "'+fixture_name+'"',1)
 assert '[lib]' not in body
 body+='\n[lib]\nname = "mywhisper"\n'
 f.write_text(body)
 f=p/'Cargo.lock';body=f.read_text();needle='name = "mywhisper"\nversion = "0.9.3"';assert body.count(needle)==1;f.write_text(body.replace(needle,'name = "'+fixture_name+'"\nversion = "0.9.3"'))
 shutil.copy2(repo/'docs/release-audit-implementation/reset_schema_probe.rs',p/'examples/reset_schema_probe.rs')
 return p
old=snapshot('baseline',False);new=snapshot('implementation',True)
def run(args,cwd,log):
 with open(log,'a') as out:
  p=subprocess.Popen(args,cwd=cwd,env=env,stdout=out,stderr=subprocess.STDOUT,start_new_session=True)
  try:code=p.wait(timeout=120)
  except subprocess.TimeoutExpired:os.killpg(p.pid,signal.SIGKILL);p.wait();raise
  assert code==0,(args,code)
for p,name in [(old,'old'),(new,'new')]:
 run(['cargo','build','--locked','--example','reset_schema_probe'],p,'/tmp/mywhisper-review-reset-compatibility-build.log')
 shutil.copy2(repo/'target/debug/examples/reset_schema_probe',root/name)
for name,mode in [('new','create'),('old','refuse'),('new','verify')]:run([str(root/name),mode],root,'/tmp/mywhisper-review-reset-compatibility.log')
print('Actual older-build refusal/current encrypted upgrade verified:',root)
