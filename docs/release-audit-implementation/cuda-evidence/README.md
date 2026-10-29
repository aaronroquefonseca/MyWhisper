The live gate uses only disposable private libraries and synthetic speech. It leaves the installed driver and CUDA unchanged. The metadata and two artifact digests are pinned to [NVIDIA CUDA 12.9.1 redistribution metadata](https://developer.download.nvidia.com/compute/cuda/redist/redistrib_12.9.1.json).

Build and rerun locally:

```sh
cargo build --locked --example runtime_smoke
MYWHISPER_AUDIT_SCRATCH=/home/aaronroque/.cache python3 scripts/cuda-smoke.py \
  --driver-archive /var/cache/pacman/pkg/nvidia-utils-615.71.09-1-x86_64_v3.pkg.tar.zst \
  --public-keyring /etc/pacman.d/gnupg/pubring.gpg
```

Use a signed driver archive matching the **currently loaded** kernel module, a trusted public keyring and sufficient scratch space. Dependencies: bubblewrap/user/PID namespaces, gpgv, bsdtar, espeak-ng, ffmpeg and nvidia-smi. The private namespace owns all server descendants. The unchanged Rust gate requires its own server PID to hold GPU memory and recognize the synthetic expected word; CPU fallback cannot satisfy the GPU check.

This is source-built Whisper Tiny CUDA acceptance. It does not establish Cohere CUDA, every other advertised model/backend/language, final packaged inference or clean NixOS desktop behavior. No proprietary library or audio payload is included in the evidence.
