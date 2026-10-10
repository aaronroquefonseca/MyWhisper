"""Isolated actual schema-6 peer/upgrade, SIGKILL migration and refusal checks."""
from pathlib import Path
import subprocess,tempfile,os,signal,shutil,time
repo=Path(__file__).resolve().parents[2]
root=Path(tempfile.mkdtemp(prefix='mywhisper-pages-compat-'))
(root/'.disposable').touch(mode=0o600)
env={**os.environ,'CARGO_TARGET_DIR':str(repo/'target'),'MYWHISPER_PAGES_COMPAT_ROOT':str(root),'MYWHISPER_DB_KEY':'2a'*32,'HOME':str(root/'home'),'XDG_CONFIG_HOME':str(root/'config'),'XDG_DATA_HOME':str(root/'data'),'XDG_CACHE_HOME':str(root/'cache')}
# Keep Cargo's installed toolchains, while every application path stays private.
env['RUSTUP_HOME']=os.environ.get('RUSTUP_HOME',str(Path.home()/'.rustup'))
env['CARGO_HOME']=os.environ.get('CARGO_HOME',str(Path.home()/'.cargo'))
for field in ['HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_CACHE_HOME']:Path(env[field]).mkdir(mode=0o700)
log=Path('/tmp/mywhisper-review-pages-compatibility.log');build=Path('/tmp/mywhisper-review-pages-compatibility-build.log')
log.write_text('');build.write_text('')
changed=subprocess.check_output(['git','diff','--name-only'],cwd=repo,text=True).splitlines()+subprocess.check_output(['git','ls-files','--others','--exclude-standard'],cwd=repo,text=True).splitlines()
def snapshot(name,current):
    p=root/name;p.mkdir()
    archive=subprocess.Popen(['git','archive','HEAD' if current else '6df7b23'],cwd=repo,stdout=subprocess.PIPE)
    subprocess.run(['tar','-x','-C',str(p)],stdin=archive.stdout,check=True);archive.stdout.close();assert archive.wait()==0
    if current:
        for file in changed:
            source=repo/file
            if source.is_file():(p/file).parent.mkdir(parents=True,exist_ok=True);shutil.copy2(source,p/file)
        # Snapshot-only deterministic kill point, after the real migration's
        # SQL executes and before its transaction commits; never shipped.
        f=p/'src/store.rs';s=f.read_text();needle='pages::initialize(&tx)?;'
        hook='''pages::initialize(&tx)?;
            if let Some(marker)=std::env::var_os("MYWHISPER_MIGRATION_KILL_MARKER") {
                let marker=std::path::PathBuf::from(marker);
                let root=std::path::PathBuf::from(std::env::var_os("MYWHISPER_PAGES_COMPAT_ROOT").unwrap());
                assert!(root.join(".disposable").is_file() && marker.parent()==Some(root.as_path()));
                std::fs::write(marker,b"transaction-uncommitted").unwrap();
                loop {std::thread::park();}
            }'''
        assert s.count(needle)==2;f.write_text(s.replace(needle,hook))
    fixture='mywhisper-pages-compat-'+name
    f=p/'Cargo.toml';s=f.read_text().replace('name = "mywhisper"','name = "'+fixture+'"',1);assert '[lib]' not in s;f.write_text(s+'\n[lib]\nname = "mywhisper"\n')
    f=p/'Cargo.lock';s=f.read_text();needle='name = "mywhisper"\nversion = "0.9.3"';assert s.count(needle)==1;f.write_text(s.replace(needle,'name = "'+fixture+'"\nversion = "0.9.3"'))
    example='pages_compat_'+name
    shutil.copy2(repo/'docs/release-audit-implementation/pages_compat_probe.rs',p/'examples'/f'{example}.rs')
    run(['cargo','build','--locked','--example',example],p,build)
    shutil.copy2(repo/'target/debug/examples'/example,root/(name+'-probe'))
    return root/(name+'-probe')
def run(args,cwd=root,destination=log,extra=None):
    with destination.open('a') as out:
        child=subprocess.Popen(args,cwd=cwd,env={**env,**(extra or {})},stdout=out,stderr=subprocess.STDOUT,start_new_session=True)
        try:code=child.wait(timeout=120)
        except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait();raise
        assert code==0,(args,code,destination)
def wait_file(path,child):
    deadline=time.monotonic()+30
    while not path.exists():
        assert child.poll() is None,(child.returncode,log)
        assert time.monotonic()<deadline,path
        time.sleep(.01)
old=snapshot('baseline',False);new=snapshot('implementation',True)
for binary,node in [(old,'old'),(new,'new')]:run([str(binary),'create',node])
for binary,node in [(old,'old'),(new,'new')]:run([str(binary),'trust',node])
for server,server_node,client,client_node in [(old,'old',new,'new'),(new,'new',old,'old')]:
    with log.open('a') as out:
        child=subprocess.Popen([str(server),'serve',server_node],cwd=root,env=env,stdout=out,stderr=subprocess.STDOUT,start_new_session=True)
        try:
            wait_file(root/f'{server_node}.address',child)
            run([str(client),'dial',client_node])
            (root/f'{server_node}.stop').touch()
            assert child.wait(timeout=15)==0
        finally:
            if child.poll() is None:os.killpg(child.pid,signal.SIGKILL);child.wait()
# Interrupt the real schema-6 migration after all its SQL, before COMMIT.
marker=root/'migration-kill-marker'
with log.open('a') as out:
    child=subprocess.Popen([str(new),'upgrade','old'],cwd=root,env={**env,'MYWHISPER_MIGRATION_KILL_MARKER':str(marker)},stdout=out,stderr=subprocess.STDOUT,start_new_session=True)
    try:
        wait_file(marker,child);os.killpg(child.pid,signal.SIGKILL);assert child.wait(timeout=10)==-signal.SIGKILL
    finally:
        if child.poll() is None:os.killpg(child.pid,signal.SIGKILL);child.wait()
# The actual old binary can still open and mutate the uncommitted schema 6.
run([str(old),'trust','old'])
run([str(new),'upgrade','old'])
run([str(old),'refuse','old'])
run([str(new),'verify','old'])
print('Legacy wire compatibility, killed migration recovery and downgrade refusal passed:',root)
