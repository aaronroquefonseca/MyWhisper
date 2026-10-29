from pathlib import Path
import subprocess,os,time,shutil,ctypes,ctypes.util,sys
root=Path(os.environ['MYWHISPER_AUDIT_ROOT'])
assert root.name.startswith('mywhisper-artifactnative-audit.') and (root/'.disposable').is_file()
artifact=Path(sys.argv[1]); assert artifact.is_file() and str(artifact).startswith('/nix/store/')
env=os.environ.copy();env['MYWHISPER_DB_KEY']='00'*32
with (root/'application.log').open('wb') as log:
 process=subprocess.Popen([str(artifact)],env=env,stdout=log,stderr=log)
 try:
  deadline=time.monotonic()+15
  while True:
   if process.poll() is not None:raise RuntimeError('packaged artifact exited before its window became visible')
   found=subprocess.run(['xdotool','search','--onlyvisible','--pid',str(process.pid)],capture_output=True,text=True,timeout=2)
   if found.returncode==0 and found.stdout.strip():window=int(found.stdout.split()[0]);break
   if time.monotonic()>=deadline:raise RuntimeError('packaged native window deadline exceeded')
   time.sleep(.05)
  values={}
  for field in Path(f'/proc/{process.pid}/environ').read_bytes().split(b'\0'):
   if b'=' in field:
    key,value=field.split(b'=',1)
    if key in (b'PATH',b'LD_LIBRARY_PATH'):values[key.decode()]=value.decode()
  resolver=shutil.which('getent',path=values['PATH'])
  assert resolver and resolver.startswith('/nix/store/')
  resolution=subprocess.run([resolver,'ahosts','localhost'],env={**env,**values},capture_output=True,timeout=5)
  assert resolution.returncode==0 and b'127.0.0.1' in resolution.stdout
  class Data(ctypes.Union):_fields_=[('b',ctypes.c_char*20),('s',ctypes.c_short*10),('l',ctypes.c_long*5)]
  class Message(ctypes.Structure):_fields_=[('type',ctypes.c_int),('serial',ctypes.c_ulong),('send_event',ctypes.c_int),('display',ctypes.c_void_p),('window',ctypes.c_ulong),('message_type',ctypes.c_ulong),('format',ctypes.c_int),('data',Data)]
  class Event(ctypes.Union):_fields_=[('message',Message),('pad',ctypes.c_long*24)]
  x=ctypes.CDLL(ctypes.util.find_library('X11'))
  x.XOpenDisplay.argtypes=[ctypes.c_char_p];x.XOpenDisplay.restype=ctypes.c_void_p
  x.XInternAtom.argtypes=[ctypes.c_void_p,ctypes.c_char_p,ctypes.c_int];x.XInternAtom.restype=ctypes.c_ulong
  x.XSendEvent.argtypes=[ctypes.c_void_p,ctypes.c_ulong,ctypes.c_int,ctypes.c_long,ctypes.POINTER(Event)]
  x.XFlush.argtypes=[ctypes.c_void_p];x.XCloseDisplay.argtypes=[ctypes.c_void_p]
  display=x.XOpenDisplay(os.environ['DISPLAY'].encode());assert display
  event=Event();event.message.type=33;event.message.send_event=1;event.message.display=display;event.message.window=window;event.message.message_type=x.XInternAtom(display,b'WM_PROTOCOLS',0);event.message.format=32;event.message.data.l[0]=x.XInternAtom(display,b'WM_DELETE_WINDOW',0)
  assert x.XSendEvent(display,window,0,0,ctypes.byref(event))!=0
  x.XFlush(display);x.XCloseDisplay(display)
  assert process.wait(timeout=10)==0,'packaged artifact did not close normally'
  databases=list((root/'data/mywhisper').glob('*.db'))
  assert len(databases)==1 and databases[0].stat().st_size>0
  assert databases[0].read_bytes()[:16]!=b'SQLite format 3\0'
 except Exception:
  log.flush()
  print((root/"application.log").read_text(errors="replace")[-12000:],flush=True)
  raise
 finally:
  if process.poll() is None:process.kill();process.wait(timeout=3)
print('Actual Nix-packaged GTK artifact passed: visible window, executable resolver from actual wrapper PATH, normal WM_DELETE_WINDOW/zero exit, encrypted disposable database; no input/audio devices.',flush=True)
