#!/usr/bin/env python3
"""MyWhisper Qwen3-ASR sidecar server.

Serves a locally downloaded Qwen3-ASR snapshot over loopback HTTP for
the MyWhisper Rust engine. One model per process (model dir + backend
are startup flags, mirroring how whisper-server/sherpa servers are
spawned per model).

Backends (upstream `qwen-asr` package):
  transformers  CPU or CUDA, whole-utterance decode only.
  vllm          NVIDIA GPU, whole-utterance + live streaming.

Protocol (JSON everywhere; audio is base64 f32le mono):
  GET  /health                                   -> {"status":"ok",...}
  POST /inference  {pcm_b64, sample_rate, language, context}
                                                -> {"text": ...}
  POST /stream/start  {language, context}        -> {"session": id}
  POST /stream/chunk  {session, pcm_b64}         -> {"text": partial}
  POST /stream/finish {session}                  -> {"text": final}

`language` is an ISO code ("en", "zh", "auto", ...); it is mapped to
the Qwen language names the `transcribe()` API expects, with "auto"
(and any unmapped code) meaning detect. `context` is the dictionary
bias list (upstream `transcribe(context=[...])`); streaming passes it
to `init_streaming_state` on a best-effort basis.

Only stdlib + numpy + qwen_asr (+ torch/vllm via qwen_asr) are used,
so the provisioned venv needs nothing beyond the runtime pack.
"""

import argparse
import base64
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np

# ISO code -> Qwen3-ASR language name (model card's 30 languages).
# Unmapped codes fall back to auto-detect (None).
LANG_NAMES = {
    "en": "English",
    "zh": "Chinese",
    "yue": "Cantonese",
    "ar": "Arabic",
    "de": "German",
    "fr": "French",
    "es": "Spanish",
    "pt": "Portuguese",
    "id": "Indonesian",
    "it": "Italian",
    "ko": "Korean",
    "ru": "Russian",
    "th": "Thai",
    "vi": "Vietnamese",
    "ja": "Japanese",
    "tr": "Turkish",
    "hi": "Hindi",
    "ms": "Malay",
    "nl": "Dutch",
    "sv": "Swedish",
    "da": "Danish",
    "fi": "Finnish",
    "pl": "Polish",
    "cs": "Czech",
    "fil": "Filipino",
    "fa": "Persian",
    "el": "Greek",
    "hu": "Hungarian",
    "mk": "Macedonian",
    "ro": "Romanian",
}


def qwen_language(code):
    if not code or code == "auto":
        return None
    return LANG_NAMES.get(code.strip().lower())


def decode_pcm(b64, sample_rate):
    raw = base64.b64decode(b64 or "")
    pcm = np.frombuffer(raw, dtype=np.float32).astype(np.float32, copy=False)
    return np.asarray(pcm, dtype=np.float32), int(sample_rate or 16000)


def load_model(model_dir, backend):
    from qwen_asr import Qwen3ASRModel

    if backend == "vllm":
        import os

        # Shared-GPU operators can raise this (default 0.5) when the
        # card is mostly free; the Rust probe surfaces vLLM's own
        # free-memory error when it still does not fit. 0.5 keeps a
        # background dictation engine a good citizen on desktops
        # (compositor/other apps share the card) while covering the
        # weights plus a dictation-sized KV cache several times over.
        try:
            mem_util = float(os.environ.get("QWEN_GPU_MEM_UTIL", "0.5"))
        except ValueError:
            mem_util = 0.5
        mem_util = min(max(mem_util, 0.1), 0.95)
        print(
            f"[qwen] loading {model_dir} (vllm backend, gpu_mem={mem_util})", flush=True
        )
        kwargs = dict(
            model=model_dir,
            gpu_memory_utilization=mem_util,
            max_new_tokens=1024,
        )
        # Shared-GPU operators can cap the context (default is the
        # upstream max); dictation utterances are short.
        max_len = os.environ.get("QWEN_MAX_MODEL_LEN", "").strip()
        if max_len:
            kwargs["max_model_len"] = int(max_len)
        return Qwen3ASRModel.LLM(**kwargs)
    import torch

    if torch.cuda.is_available():
        dtype, device = torch.bfloat16, "cuda:0"
    else:
        # bfloat16 CPU support is spotty; float32 always decodes.
        dtype, device = torch.float32, "cpu"
    print(f"[qwen] loading {model_dir} (transformers, {device})", flush=True)
    return Qwen3ASRModel.from_pretrained(
        model_dir,
        dtype=dtype,
        device_map=device,
        max_new_tokens=1024,
    )


def transcribe_offline(model, pcm, sr, language, context):
    """Whole-utterance decode with forced-language + context retries.

    A forced language the model build rejects must never fail the
    utterance: retry as auto-detect, then without context.
    """
    attempts = []
    if context:
        attempts.append({"language": language, "context": [context]})
    attempts.append({"language": language})
    attempts.append({})
    last = None
    for kw in attempts:
        try:
            results = model.transcribe(audio=(pcm, sr), return_time_stamps=False, **kw)
            return results[0].text or "", results[0].language
        except TypeError as e:
            # Signature drift (e.g. no `context` kwarg): next attempt
            # drops a kwarg instead of failing the utterance.
            last = e
            print(f"[qwen] transcribe kwargs rejected ({e}); retrying", flush=True)
        except Exception as e:  # noqa: BLE001 - surfaced as HTTP 500
            last = e
            print(f"[qwen] transcribe failed ({e}); retrying", flush=True)
    raise RuntimeError(f"transcribe failed: {last}")


class State:
    def __init__(self, model, backend):
        self.model = model
        self.backend = backend
        self.lock = threading.Lock()
        self.sessions = {}
        self.next_id = 1


STATE: "State | None" = None  # set in main() before serving


def state():
    assert STATE is not None, "server started without state"
    return STATE


class Handler(BaseHTTPRequestHandler):
    server_version = "mywhisper-qwen/1"

    def log_message(self, format, *args):  # noqa: A002 - stdlib signature
        sys.stderr.write("[qwen] " + format % args + "\n")

    def _json(self, obj, status=200):
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _body(self):
        try:
            length = int(self.headers.get("Content-Length", 0))
        except ValueError:
            length = 0
        raw = self.rfile.read(length) if length > 0 else b"{}"
        try:
            return json.loads(raw.decode() or "{}")
        except ValueError:
            return {}

    def do_GET(self):  # noqa: N802 - stdlib signature
        if self.path == "/health":
            self._json({"status": "ok", "backend": state().backend})
        else:
            self._json({"error": "unknown path"}, 404)

    def do_POST(self):  # noqa: N802 - stdlib signature
        st = state()
        try:
            if self.path == "/inference":
                req = self._body()
                pcm, sr = decode_pcm(
                    req.get("pcm_b64", ""), req.get("sample_rate", 16000)
                )
                lang = qwen_language(req.get("language", "auto"))
                text, _ = transcribe_offline(
                    st.model, pcm, sr, lang, (req.get("context") or "").strip()
                )
                self._json({"text": text})
            elif self.path == "/stream/start":
                if st.backend != "vllm":
                    self._json({"error": "streaming needs the vLLM runtime"}, 400)
                    return
                req = self._body()
                lang = qwen_language(req.get("language", "auto"))
                ctx = (req.get("context") or "").strip()
                with st.lock:
                    sid = str(st.next_id)
                    st.next_id += 1
                    kwargs = dict(
                        unfixed_chunk_num=2, unfixed_token_num=5, chunk_size_sec=2.0
                    )
                    try:
                        if ctx:
                            stream = st.model.init_streaming_state(
                                context=[ctx], **kwargs
                            )
                        else:
                            stream = st.model.init_streaming_state(**kwargs)
                    except TypeError:
                        stream = st.model.init_streaming_state(**kwargs)
                    st.sessions[sid] = {"stream": stream, "language": lang}
                self._json({"session": sid})
            elif self.path == "/stream/chunk":
                req = self._body()
                with st.lock:
                    sess = st.sessions.get(req.get("session", ""))
                if sess is None:
                    self._json({"error": "unknown session"}, 404)
                    return
                pcm, _sr = decode_pcm(req.get("pcm_b64", ""), 16000)
                if pcm.size == 0:
                    self._json({"text": ""})
                    return
                # Streaming consumes incrementally; language/context
                # were fixed at /stream/start.
                st.model.streaming_transcribe(pcm, sess["stream"])
                self._json({"text": sess["stream"].text or ""})
            elif self.path == "/stream/finish":
                req = self._body()
                with st.lock:
                    sess = st.sessions.pop(req.get("session", ""), None)
                if sess is None:
                    self._json({"error": "unknown session"}, 404)
                    return
                st.model.finish_streaming_transcribe(sess["stream"])
                self._json({"text": sess["stream"].text or ""})
            else:
                self._json({"error": "unknown path"}, 404)
        except Exception as e:  # noqa: BLE001 - every failure is a 500
            self._json({"error": str(e)}, 500)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--backend", choices=["transformers", "vllm"], required=True)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    args = ap.parse_args()
    # Model loads before serving: TCP-ready means ready to decode
    # (same contract as whisper-server).
    model = load_model(args.model, args.backend)
    global STATE
    STATE = State(model, args.backend)
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"[qwen] serving on {args.host}:{args.port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
