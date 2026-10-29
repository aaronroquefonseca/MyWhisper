import os, subprocess, tempfile, sys
from pathlib import Path
store=Path('/home/aaronroque/.cache/mywhisper-nix-owned-store.p3jfxm4a/nix')
assert store.parent.stat().st_uid==os.getuid() and (store.parent/'.disposable').is_file()
assert store.parent.stat().st_mode & 0o777 == 0o700
root=Path(tempfile.mkdtemp(prefix='mywhisper-nix-gate.',dir='/home/aaronroque/.cache'));root.chmod(0o700);(root/'.disposable').touch(mode=0o600)
for name in ('home','config','cache','state'): (root/name).mkdir(mode=0o700)
build=(root/'build') if '--file' in sys.argv[2:] else Path('target/nix-audit-build').resolve();build.mkdir(exist_ok=True)
mapped_build='/tmp'
env={**os.environ,'HOME':str(root/'home'),'XDG_CONFIG_HOME':str(root/'config'),'XDG_CACHE_HOME':str(root/'cache'),'XDG_STATE_HOME':str(root/'state'),'NIX_REMOTE':'local','NIX_CONFIG':'experimental-features = nix-command flakes\nbuild-users-group =\nsandbox = false\nmax-jobs = 1\ncores = 4\nbuild-dir = '+mapped_build+'\n'}
args=sys.argv[2:]
command=['bwrap','--ro-bind','/','/','--bind',str(store),'/nix','--bind',str(build),mapped_build,'--bind',str(root),str(root),'--unshare-user','--unshare-pid','--proc','/proc','--dev','/dev','--die-with-parent','--new-session']
# Bind host KVM whenever it is accessible, independent of the check name:
# the old '.desktop'-suffix heuristic silently dropped KVM for any other VM
# check (e.g. clipboard-guest), failing its build with missing features.
# Conditional (not assert): KVM-less hosts still run non-VM derivations.
if os.access('/dev/kvm',os.R_OK|os.W_OK):
 command+=['--dev-bind','/dev/kvm','/dev/kvm']
command+=['--','/usr/bin/nix']+args
print('Task-owned gate root',root,flush=True)
with open(sys.argv[1],'w') as log:
 result=subprocess.run(command,env=env,stdout=log,stderr=subprocess.STDOUT)
print('Gate exit',result.returncode,flush=True)
raise SystemExit(result.returncode)
