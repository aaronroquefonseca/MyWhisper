#!/usr/bin/env python3
"""Drive the real CPAL/ALSA backend with a process-private virtual microphone.

ALSA's file/null PCM reads only this runner's paced synthetic FIFO. No physical
microphone, host default source, settings store or Secret Service is accessed.
"""
import os
from pathlib import Path
import subprocess
import sys

root = Path(os.environ["MYWHISPER_AUDIT_ROOT"])
if not (root / ".disposable").is_file() or root.stat().st_mode & 0o777 != 0o700:
    raise SystemExit("native capture gate requires disposable state")
binary, sample_format, channels = sys.argv[1:]
os.mkfifo(root / "synthetic.fifo", mode=0o600)
# Configuration uses no host ALSA hooks/defaults: the only defined PCM is
# this process's file source, whose embedded null slave has no audio device.
(root / "alsa.conf").write_text('''
pcm.default {
  type file
  slave.pcm { type null }
  file "/dev/null"
  infile "''' + str(root / "synthetic.fifo") + '''"
  format "raw"
  hint.description "MyWhisper disposable synthetic microphone"
}
''')
env = dict(os.environ, ALSA_CONFIG_PATH=str(root / "alsa.conf"),
           MYWHISPER_CPAL_NATIVE_GATE=str(root), MYWHISPER_CPAL_FORMAT=sample_format,
           MYWHISPER_CPAL_CHANNELS=channels)
sys.exit(subprocess.run([binary, "--exact", "mic::format_regressions::interleaved_frames_downmix_without_changing_duration_or_order",
                        "--test-threads=1", "--nocapture"], env=env, timeout=20).returncode)
