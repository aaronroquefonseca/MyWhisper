from pathlib import Path
import os,subprocess,tempfile
repo=Path.cwd()
archive=Path('/var/cache/pacman/pkg/nvidia-utils-615.71.09-1-x86_64_v3.pkg.tar.zst')
version=Path('/sys/module/nvidia/version').read_text().strip()
if version!='615.71.09':raise RuntimeError('loaded driver no longer matches the signed acceptance archive')
with tempfile.TemporaryDirectory(prefix='mywhisper-cohere-cuda-audit.',dir='/home/aaronroque/.cache') as directory:
 root=Path(directory);root.chmod(0o700);(root/'.disposable').touch(mode=0o600)
 library=root/'lib';library.mkdir(mode=0o700)
 gpg=root/'gpg';gpg.mkdir(mode=0o700)
 temporary=root/'tmp';temporary.mkdir(mode=0o700)
 subprocess.run(['gpgv','--homedir',str(gpg),'--keyring','/etc/pacman.d/gnupg/pubring.gpg',str(archive)+'.sig',str(archive)],check=True,timeout=30)
 for name in ['libcuda','libnvidia-ml','libnvidia-ptxjitcompiler','libnvidia-gpucomp']:
  filename=f'{name}.so.{version}'
  with (library/filename).open('xb') as output:subprocess.run(['bsdtar','-xOf',str(archive),'usr/lib/'+filename],stdout=output,check=True,timeout=30)
  (library/(name+('.so' if name=='libnvidia-gpucomp' else '.so.1'))).symlink_to(filename)
 env=os.environ.copy();env['LD_LIBRARY_PATH']=str(library)+':/opt/cuda/lib64:/usr/lib';env['TMPDIR']=str(temporary)
 subprocess.run(['nvidia-smi','--query-gpu=name,driver_version','--format=csv,noheader'],env=env,check=True,timeout=10)
 subprocess.run(['pacman','-Q','cuda','cudnn'],check=True,timeout=10)
 wav=root/'synthetic.wav';pcm=root/'synthetic.raw'
 subprocess.run(['espeak-ng','-w',str(wav),'Hello world. This is a test of speech recognition.'],check=True,timeout=10)
 subprocess.run(['ffmpeg','-nostdin','-v','error','-i',str(wav),'-ar','16000','-ac','1','-f','s16le',str(pcm)],check=True,timeout=10)
 print('Actual Cohere/CUDA model download, runtime readiness, server-owned GPU memory, synthetic inference and cleanup gate begins.',flush=True)
 subprocess.run(['python3',str(repo/'scripts/disposable.py'),'MYWHISPER_AUDIT_ROOT','coherecuda',str(repo/'target/debug/examples/runtime_smoke'),'cohere-transcribe-03-2026','cuda',str(pcm),'hello'],env=env,check=True,timeout=135)
print('Actual Cohere CUDA gate passed; private driver/model/synthetic-audio state removed; installed libraries untouched.',flush=True)
