#!/usr/bin/env python3
"""FastWord MLX Whisper sidecar.

Reads line-delimited JSON from stdin, transcribes Float32 PCM audio,
writes line-delimited JSON to stdout. Keeps the model hot in RAM and
evicts it after IDLE_EVICT_SECONDS of inactivity.
"""
from __future__ import annotations

import base64
import json
import os
import pathlib
import struct
import sys
import threading
import time
import traceback
from typing import Any

import numpy as np

MODEL_REPO = os.environ.get("FASTWORD_MODEL", "mlx-community/whisper-large-v3-turbo")
IDLE_EVICT_SECONDS = float(os.environ.get("FASTWORD_IDLE_EVICT", "600"))
LANGUAGE = os.environ.get("FASTWORD_LANGUAGE")  # None = auto

LOG_PATH = pathlib.Path.home() / ".fastword" / "sidecar.log"
LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
_LOG_FH = LOG_PATH.open("a", buffering=1)


def log(msg: str) -> None:
    line = f"[{time.strftime('%H:%M:%S')}] {msg}"
    print(line, file=sys.stderr, flush=True)
    _LOG_FH.write(line + "\n")


class ModelHolder:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._loaded_at: float = 0.0
        self._mlx_whisper = None

    def transcribe(self, audio: np.ndarray) -> dict[str, Any]:
        with self._lock:
            if self._mlx_whisper is None:
                log(f"loading model {MODEL_REPO}")
                import mlx_whisper  # type: ignore
                self._mlx_whisper = mlx_whisper
                log("model loaded")
            self._loaded_at = time.time()
            kwargs: dict[str, Any] = {"path_or_hf_repo": MODEL_REPO}
            if LANGUAGE:
                kwargs["language"] = LANGUAGE
            return self._mlx_whisper.transcribe(audio, **kwargs)

    def maybe_evict(self) -> None:
        with self._lock:
            if self._mlx_whisper is None:
                return
            if time.time() - self._loaded_at > IDLE_EVICT_SECONDS:
                log("evicting idle model")
                self._mlx_whisper = None
                import gc
                gc.collect()


def decode_pcm(b64: str) -> np.ndarray:
    raw = base64.b64decode(b64)
    n = len(raw) // 4
    floats = struct.unpack(f"<{n}f", raw[: n * 4])
    return np.asarray(floats, dtype=np.float32)


def handle(req: dict[str, Any], holder: ModelHolder) -> dict[str, Any]:
    rid = req.get("id", "")
    cmd = req.get("cmd")
    if cmd == "warmup":
        try:
            # 0.5s of silence to force model load if not already loaded.
            silence = np.zeros(8000, dtype=np.float32)
            holder.transcribe(silence)
            return {"id": rid, "text": ""}
        except Exception as exc:  # noqa: BLE001
            log(f"warmup error: {exc!r}")
            return {"id": rid, "error": str(exc)}
    if cmd != "transcribe":
        return {"id": rid, "error": f"unknown cmd: {cmd}"}
    try:
        audio = decode_pcm(req["audio_b64"])
        if audio.size < 1600:  # under 0.1s, ignore
            return {"id": rid, "text": ""}
        result = holder.transcribe(audio)
        text = (result.get("text") or "").strip()
        return {"id": rid, "text": text}
    except Exception as exc:  # noqa: BLE001
        log(f"error: {exc!r}")
        return {"id": rid, "error": str(exc)}


def evict_loop(holder: ModelHolder, stop: threading.Event) -> None:
    while not stop.wait(30):
        holder.maybe_evict()


def main() -> int:
    holder = ModelHolder()
    stop = threading.Event()
    threading.Thread(target=evict_loop, args=(holder, stop), daemon=True).start()
    log("ready")
    try:
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            try:
                req = json.loads(line)
            except json.JSONDecodeError as exc:
                sys.stdout.write(json.dumps({"id": "", "error": f"bad json: {exc}"}) + "\n")
                sys.stdout.flush()
                continue
            resp = handle(req, holder)
            sys.stdout.write(json.dumps(resp, ensure_ascii=False) + "\n")
            sys.stdout.flush()
    finally:
        stop.set()
    return 0


if __name__ == "__main__":
    sys.exit(main())
