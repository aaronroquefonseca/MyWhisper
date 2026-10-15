#!/usr/bin/env python3
"""Real Whisper CUDA acceptance using disposable, authenticated prerequisites.

Supply a signed Arch driver archive matching the loaded kernel module and a
trusted public keyring. Set MYWHISPER_AUDIT_SCRATCH to a scratch parent on disk
if /tmp has insufficient space. No system package is installed or changed.
Build runtime_smoke first. Libraries, models and synthetic audio are disposable.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request

BASE = "https://developer.download.nvidia.com/compute/cuda/redist/"
PREREQUISITES = {
    "cuda_cudart": (
        "cuda_cudart/linux-x86_64/cuda_cudart-linux-x86_64-12.9.79-archive.tar.xz",
        "1f6ad42d4f530b24bfa35894ccf6b7209d2354f59101fd62ec4a6192a184ce99",
        1514676,
    ),
    "libcublas": (
        "libcublas/linux-x86_64/libcublas-linux-x86_64-12.9.1.4-archive.tar.xz",
        "546addc4a9d82b8f23aa9ba9274b6bc0429a63008a31c759884ac24880796057",
        933611504,
    ),
}


def require(condition, message):
    # Integrity and ownership checks also apply when Python runs with -O.
    if not condition:
        raise RuntimeError(message)


def download_prerequisite(name, item, root, library):
    relative_path, digest, expected = PREREQUISITES[name]
    require(
        (item["relative_path"], item["sha256"], int(item["size"]))
        == (relative_path, digest, expected),
        "published CUDA metadata differs from pinned acceptance prerequisites",
    )
    archive = root / (name + ".tar.xz")
    checksum = hashlib.sha256()
    received = 0
    started = reported = time.monotonic()
    print("Downloading authenticated CUDA prerequisite:", name, expected, "bytes", flush=True)
    with urllib.request.urlopen(BASE + relative_path, timeout=20) as response, archive.open("xb") as output:
        while chunk := response.read1(1024 * 1024):
            received += len(chunk)
            require(received <= expected, "CUDA archive exceeds pinned size")
            require(time.monotonic() - started < 600, "CUDA prerequisite download deadline exceeded")
            output.write(chunk)
            checksum.update(chunk)
            if time.monotonic() - reported > 10:
                print(name, received, "downloaded bytes; authentication pending", flush=True)
                reported = time.monotonic()
    require(received == expected and checksum.hexdigest() == digest, "CUDA archive authentication failed")
    print("SHA-256 and exact size verified:", name, digest, flush=True)

    links = []
    extracted = 0
    started = time.monotonic()
    with tarfile.open(archive, "r|xz") as source:
        for count, member in enumerate(source):
            require(count < 256 and time.monotonic() - started < 600, "CUDA archive extraction limit exceeded")
            path = PurePosixPath(member.name)
            require(not path.is_absolute() and ".." not in path.parts, "unsafe CUDA archive path")
            # Link-time stubs are not execution libraries; avoid basename collisions.
            if path.parent.name != "lib" or ".so" not in path.name:
                continue
            if member.issym():
                target = PurePosixPath(member.linkname)
                require(len(target.parts) == 1 and target.name not in (".", ".."), "unsafe library link")
                links.append((path.name, target.name))
            elif member.isfile():
                extracted += member.size
                require(0 < member.size <= 1_000_000_000 and extracted <= 2_000_000_000, "CUDA library size limit exceeded")
                with source.extractfile(member) as input_file, (library / path.name).open("xb") as output:
                    shutil.copyfileobj(input_file, output, 1024 * 1024)
            else:
                raise RuntimeError("unexpected shared-library archive member")
    # CUDA ships link chains. Create all links before checking their targets.
    for name, target in links:
        (library / name).symlink_to(target)
    for _, target in links:
        require((library / target).is_file(), "missing private library link target")
    archive.unlink()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--driver-archive", type=Path, required=True)
    parser.add_argument("--public-keyring", type=Path, required=True)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    archive = args.driver_archive.resolve(strict=True)
    public_keyring = args.public_keyring.resolve(strict=True)
    version = Path("/sys/module/nvidia/version").read_text().strip()
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) is not None, "invalid loaded NVIDIA module version")

    with tempfile.TemporaryDirectory(prefix="mywhisper-cuda-audit.", dir=os.environ.get("MYWHISPER_AUDIT_SCRATCH")) as directory:
        root = Path(directory)
        root.chmod(0o700)
        (root / ".disposable").touch(mode=0o600)
        library = root / "lib"
        library.mkdir(mode=0o700)
        gpg = root / "gpg"
        gpg.mkdir(mode=0o700)
        subprocess.run(
            ["gpgv", "--homedir", str(gpg), "--keyring", str(public_keyring), str(archive) + ".sig", str(archive)],
            check=True, timeout=30,
        )
        libraries = ["libcuda", "libnvidia-ml", "libnvidia-ptxjitcompiler", "libnvidia-gpucomp"]
        for name in libraries:
            filename = f"{name}.so.{version}"
            with (library / filename).open("xb") as output:
                subprocess.run(["bsdtar", "-xOf", str(archive), "usr/lib/" + filename], stdout=output, check=True, timeout=30)
            alias = name + (".so" if name == "libnvidia-gpucomp" else ".so.1")
            (library / alias).symlink_to(filename)
        env = os.environ.copy()
        env["LD_LIBRARY_PATH"] = str(library)
        result = subprocess.run(
            ["nvidia-smi", "--query-gpu=name,driver_version", "--format=csv,noheader"],
            env=env, capture_output=True, text=True, check=True, timeout=10,
        )
        print("Private matching library query:", result.stdout.strip(), flush=True)
        metadata_bytes = bytearray()
        started = time.monotonic()
        with urllib.request.urlopen(BASE + "redistrib_12.9.1.json", timeout=20) as response:
            while chunk := response.read1(64 * 1024):
                metadata_bytes.extend(chunk)
                require(len(metadata_bytes) <= 1024 * 1024, "CUDA metadata size limit exceeded")
                require(time.monotonic() - started < 60, "CUDA metadata deadline exceeded")
        metadata = json.loads(metadata_bytes)
        for name in PREREQUISITES:
            download_prerequisite(name, metadata[name]["linux-x86_64"], root, library)

        wav, pcm = root / "synthetic.wav", root / "synthetic.raw"
        subprocess.run(["espeak-ng", "-w", str(wav), "Hello world. This is a test of speech recognition."], check=True, timeout=10)
        subprocess.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(wav), "-ar", "16000", "-ac", "1", "-f", "s16le", str(pcm)], check=True, timeout=10)
        print("Starting actual pinned-model/download/readiness/CUDA/inference/cleanup gate", flush=True)
        subprocess.run(
            [sys.executable, str(repo / "scripts/disposable.py"), "MYWHISPER_AUDIT_ROOT", "cuda",
             str(repo / "target/debug/examples/runtime_smoke"), "tiny", "cuda", str(pcm), "hello"],
            env=env, check=True, timeout=135,
        )
    print("Actual CUDA acceptance passed; private libraries and synthetic speech removed", flush=True)


if __name__ == "__main__":
    main()
