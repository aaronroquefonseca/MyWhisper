import subprocess,os,select,time,sys
from pathlib import Path
root=Path(os.environ['MYWHISPER_AUDIT_ROOT']); assert (root/'.disposable').is_file()
read,write=os.pipe()
with (root/'xvfb.log').open('wb') as log:
    server=subprocess.Popen(['Xvfb','-displayfd',str(write),'-screen','0','1280x900x24','-nolisten','tcp'],pass_fds=(write,),stdout=log,stderr=log)
    os.close(write)
    try:
        if not select.select([read],[],[],5)[0]:raise RuntimeError('owned Xvfb readiness exceeded five seconds')
        data=os.read(read,64).strip()
        if not data.isdigit():raise RuntimeError('owned Xvfb failed to allocate a display')
        env=os.environ.copy();env['DISPLAY']=':'+data.decode();env.pop('XAUTHORITY',None)
        subprocess.run([sys.executable,'/tmp/mywhisper-model-load-artifact.py',sys.argv[1]],env=env,check=True,timeout=30)
    except Exception:
        log.flush();print((root/'xvfb.log').read_text(errors='replace'),flush=True);raise
    finally:
        os.close(read)
        server.terminate()
        try:server.wait(timeout=3)
        except subprocess.TimeoutExpired:server.kill();server.wait(timeout=3)
