"""RiWork's local speech engine: MLX speech models through mlx-audio, driven by RiWork over stdio.

RiWork installs this file beside its pinned runtime (pyproject.toml, uv.lock) and runs it as

  worker.py serve                          dictation sessions, JSON lines in and out (below)
  worker.py download MODEL_JSON            fetch one pinned model into the Hugging Face cache
  worker.py transcribe MODEL_JSON FILE     a 16-bit mono WAV through a dictation session, for checks

MODEL_JSON is one entry of models.json. Audio never leaves the Mac: `serve` loads models from the
local cache with the Hugging Face Hub offline, and only `download` uses the network.

`serve` reads
  {"op":"begin","session":N,"model":{...}}       a dictation; loads its model unless it is loaded
  {"op":"audio","session":N,"data":"<base64>"}   16 kHz mono float32 little-endian samples
  {"op":"finish","session":N}                    settle the last words, then report finished
  {"op":"cancel","session":N}
  {"op":"unload"}
and writes
  {"event":"note","session":N,"text":"Loading …"}
  {"event":"loaded","session":N}
  {"event":"heard","session":N,"text":"…"}       all of it so far, the last words still open to change
  {"event":"finished","session":N,"text":"…"}
  {"event":"failed","session":N,"problem":"model-unavailable"|"failed","detail":"…"}
It ends when stdin closes. A model no dictation used for IDLE_SECONDS is unloaded to free memory.
"""
from __future__ import annotations

import base64
from dataclasses import dataclass
import gc
import json
import os
from pathlib import Path
import queue
import sys
import threading
import time
import wave

import numpy as np

SAMPLE_RATE = 16_000
FRAME = 320  # 20 ms
IDLE_SECONDS = 600
# Audio kept for a dictation whose speech never opened the activity gate (a quiet microphone).
FALLBACK_SECONDS = 30
# The loudest 20 ms of that audio must reach this RMS to be decoded: a silent room is not sent
# to a model (Whisper in particular invents words for silence).
FALLBACK_FLOOR = 0.0015


@dataclass
class Chunk:
    utterance: int
    revision: int
    samples: np.ndarray
    final: bool


class Segmenter:
    """Splits one microphone stream into phrases: an energy gate with pre-roll and a silence tail.

    A phrase ends after a pause or at `max_seconds` and is emitted as final. While it grows it is
    emitted again every `draft_seconds` as a draft, so the model can correct its earlier words.
    Adapted from riTranscribe's segmenter; split decisions happen on 20 ms frames.
    """

    def __init__(self, emit, *, threshold=0.005, silence_seconds=0.8, max_seconds=20.0,
                 draft_seconds=1.5, min_voiced_seconds=0.18):
        self.emit = emit
        self.threshold = threshold
        self.silence_samples = int(silence_seconds * SAMPLE_RATE)
        self.max_samples = int(max_seconds * SAMPLE_RATE)
        self.draft_samples = max(FRAME, int(draft_seconds * SAMPLE_RATE))
        self.min_voiced = int(min_voiced_seconds * SAMPLE_RATE)
        self.utterance = 0
        self.revision = 0
        self.next_draft = self.draft_samples
        self.preroll: list[np.ndarray] = []
        self.frames: list[np.ndarray] = []
        self.count = self.voiced = self.silence = 0
        self.pending = np.empty(0, dtype=np.float32)

    def feed(self, samples):
        samples = np.nan_to_num(np.asarray(samples, dtype=np.float32).reshape(-1), nan=0, posinf=0, neginf=0)
        data = np.concatenate((self.pending, samples))
        whole = len(data) // FRAME * FRAME
        for position in range(0, whole, FRAME):
            self._frame(data[position:position + FRAME].copy())
        self.pending = data[whole:].copy()

    def _frame(self, frame):
        active = float(np.sqrt(np.mean(frame * frame))) >= self.threshold
        if not self.frames and active:
            self.utterance += 1
            self.revision = 0
            self.next_draft = self.draft_samples
            self.frames = list(self.preroll)
            self.count = sum(len(item) for item in self.frames)
            self.preroll = []
        if self.frames or active:
            self.frames.append(frame)
            self.count += len(frame)
            if active:
                self.voiced += len(frame)
                self.silence = 0
            else:
                self.silence += len(frame)
            if self.silence >= self.silence_samples or self.count >= self.max_samples:
                self.flush(final=False)
            elif self.count >= self.next_draft:
                if self.voiced >= self.min_voiced:
                    self.emit(self._chunk(final=False))
                self.next_draft += self.draft_samples
        else:
            self.preroll = (self.preroll + [frame])[-10:]  # 200 ms

    def flush(self, final=True):
        """Ends the phrase in progress; `final` also takes the samples short of a frame (stopping)."""
        if final and len(self.pending) and self.frames:
            self.frames.append(self.pending)
            self.count += len(self.pending)
        if self.frames and self.voiced >= self.min_voiced:
            self.emit(self._chunk(final=True))
        self.frames = []
        self.preroll = []
        self.count = self.voiced = self.silence = 0
        if final:
            self.pending = np.empty(0, dtype=np.float32)

    def _chunk(self, *, final):
        self.revision += 1
        return Chunk(self.utterance, self.revision, np.concatenate(self.frames), final)


class ModelMissing(Exception):
    """The model is not (completely) in the local cache."""


class Session:
    def __init__(self, session_id, spec, emit_chunk):
        self.id = session_id
        self.spec = spec
        self.finals: dict[int, str] = {}
        self.draft: tuple[int, int, str] | None = None
        self.cancelled = self.finishing = self.failed = False
        self.recent: list[np.ndarray] = []
        self.recent_samples = 0
        self.segmenter = Segmenter(lambda chunk: emit_chunk(self, chunk),
                                   draft_seconds=float(spec.get("draft_seconds") or 1.5))

    def feed(self, samples):
        self.recent.append(samples)
        self.recent_samples += len(samples)
        while self.recent and self.recent_samples - len(self.recent[0]) >= FALLBACK_SECONDS * SAMPLE_RATE:
            self.recent_samples -= len(self.recent.pop(0))
        self.segmenter.feed(samples)

    def text(self):
        words = [text for _, text in sorted(self.finals.items()) if text]
        if self.draft and self.draft[0] not in self.finals and self.draft[2]:
            words.append(self.draft[2])
        return " ".join(words)

    def draft_is_current(self, chunk):
        return (not (self.cancelled or self.finishing or self.failed) and chunk.utterance not in self.finals
                and chunk.utterance == self.segmenter.utterance
                and not (self.draft and self.draft[0] == chunk.utterance and self.draft[1] >= chunk.revision))


class Worker:
    """Owns the one loaded model. Every MLX call happens on its inference thread; finals and finish
    marks are processed in order, and each session's newest draft runs when nothing else waits."""

    def __init__(self, write, *, loader=None, transcriber=None, idle_seconds=IDLE_SECONDS, clock=time.monotonic,
                 warm_up=True):
        self.write = write
        self.warm_up = warm_up
        self.loader = loader or load_model
        self.transcriber = transcriber or transcribe
        self.idle_seconds = idle_seconds
        self.clock = clock
        self.lock = threading.Lock()
        self.jobs: queue.Queue = queue.Queue()
        self.drafts: dict[int, tuple[Session, Chunk]] = {}
        self.sessions: dict[int, Session] = {}
        self.model = None
        self.model_key = None
        self.last_used = clock()
        self.thread = threading.Thread(target=self._run, name="mlx-inference", daemon=True)
        self.thread.start()

    # Requests, on the reading thread.

    def handle(self, message):
        op, session_id = message.get("op"), message.get("session")
        if op == "unload":
            self.jobs.put(("unload", None, None))
            return
        with self.lock:
            session = self.sessions.get(session_id)
            if op == "begin":
                if session is None and isinstance(message.get("model"), dict):
                    session = Session(session_id, message["model"], self._chunk)
                    self.sessions[session_id] = session
                    self.jobs.put(("begin", session, None))
                return
            if session is None or session.cancelled:
                return
            if op == "audio":
                if not session.finishing and not session.failed:
                    data = base64.b64decode(message.get("data") or "", validate=True)
                    session.feed(np.frombuffer(data, dtype="<f4"))
            elif op == "finish":
                if not session.finishing:
                    session.finishing = True
                    self.drafts.pop(session_id, None)
                    session.segmenter.flush(final=True)
                    self.jobs.put(("finish", session, None))
            elif op == "cancel":
                session.cancelled = True
                self.sessions.pop(session_id, None)
                self.drafts.pop(session_id, None)

    def _chunk(self, session, chunk):
        # Called with the lock held, from Session.feed or the finish flush.
        if chunk.final:
            self.drafts.pop(session.id, None)
            self.jobs.put(("final", session, chunk))
        elif session.draft_is_current(chunk):
            self.drafts[session.id] = (session, chunk)

    def close(self):
        with self.lock:
            for session in self.sessions.values():
                session.cancelled = True
            self.sessions.clear()
            self.drafts.clear()
        self.jobs.put(("stop", None, None))
        self.thread.join()

    # The inference thread.

    def _next_job(self):
        try:
            return self.jobs.get_nowait()
        except queue.Empty:
            pass
        with self.lock:
            if self.drafts:
                session_id = next(iter(self.drafts))
                session, chunk = self.drafts.pop(session_id)
                return ("draft", session, chunk)
            # Drafts are polled only while someone dictates; idle, wake for the unload check.
            timeout = 0.1 if self.sessions else min(5.0, max(0.1, self.idle_seconds / 10))
        try:
            return self.jobs.get(timeout=timeout)
        except queue.Empty:
            return None

    def _run(self):
        try:
            self._loop()
        except BaseException:  # noqa: BLE001 - a worker without inference must end, so RiWork starts another
            import traceback
            traceback.print_exc()
            sys.stderr.flush()
            os._exit(1)

    def _loop(self):
        while True:
            job = self._next_job()
            if job is None:
                with self.lock:
                    idle = not self.sessions and self.clock() - self.last_used >= self.idle_seconds
                if idle and self.model is not None:
                    self._unload()
                continue
            kind, session, chunk = job
            if kind == "stop":
                self._unload()
                return
            if kind == "unload":
                with self.lock:
                    busy = bool(self.sessions)
                if not busy:
                    self._unload()
                continue
            if session.cancelled or (session.failed and kind != "finish"):
                continue
            try:
                if kind == "begin":
                    self._ensure(session)
                elif kind == "final":
                    text = self._decode(session, chunk.samples)
                    with self.lock:
                        session.finals[chunk.utterance] = text
                        if session.draft and session.draft[0] <= chunk.utterance:
                            session.draft = None
                        heard = session.text()
                    self._send(session, "heard", text=heard)
                elif kind == "draft":
                    with self.lock:
                        current = session.draft_is_current(chunk)
                    if not current:
                        continue
                    text = self._decode(session, chunk.samples)
                    with self.lock:
                        if not session.draft_is_current(chunk):
                            continue
                        session.draft = (chunk.utterance, chunk.revision, text)
                        heard = session.text()
                    self._send(session, "heard", text=heard)
                elif kind == "finish":
                    if session.failed:
                        continue
                    with self.lock:
                        quiet = not session.text()
                        recent = np.concatenate(session.recent) if session.recent else np.empty(0, np.float32)
                    if quiet and has_signal(recent):
                        # Nothing came of the phrases (a quiet microphone): decode what was captured.
                        text = self._decode(session, recent)
                        with self.lock:
                            session.finals[0] = text
                    with self.lock:
                        text = session.text()
                        self.sessions.pop(session.id, None)
                    self._send(session, "finished", text=text)
            except ModelMissing as error:
                self._fail(session, "model-unavailable", str(error))
            except Exception as error:  # noqa: BLE001 - reported to the app, which shows it
                print(f"riwork-speech-worker: {kind}: {type(error).__name__}: {error}", file=sys.stderr, flush=True)
                self._fail(session, "failed", str(error) or type(error).__name__)

    def _ensure(self, session):
        key = session.spec.get("key")
        if self.model is not None and self.model_key == key:
            self._send(session, "loaded")
            return
        if self.model is not None:
            self._unload()
        self._send(session, "note", text=f"Loading {session.spec.get('label') or key}…")
        loaded = self.loader(session.spec)
        self.model, self.model_key = loaded, key
        del loaded
        if self.warm_up:
            # The first decode compiles the model's GPU kernels (two seconds for Nemotron, a
            # tenth of that once done): pay it now, while "Loading" shows, not on the first words.
            try:
                noise = np.random.default_rng(0).normal(0, 1e-4, SAMPLE_RATE).astype(np.float32)
                self.transcriber(self.model, session.spec.get("family"), noise)
            except Exception as error:  # noqa: BLE001 - a failed warm-up only costs speed
                print(f"riwork-speech-worker: warm-up: {error}", file=sys.stderr, flush=True)
        self.last_used = self.clock()
        if not session.cancelled:
            self._send(session, "loaded")

    def _decode(self, session, samples):
        if self.model is None or self.model_key != session.spec.get("key"):
            self._ensure(session)
        text = self.transcriber(self.model, session.spec.get("family"), samples)
        self.last_used = self.clock()
        return text

    def _unload(self):
        if self.model is None:
            return
        mx = sys.modules.get("mlx.core")
        if mx is not None:
            mx.synchronize()
        self.model = None
        self.model_key = None
        gc.collect()
        if mx is not None:
            mx.clear_cache()

    def _fail(self, session, problem, detail):
        with self.lock:
            if session.failed or session.cancelled:
                return
            session.failed = True
            self.sessions.pop(session.id, None)
            self.drafts.pop(session.id, None)
        self._send(session, "failed", problem=problem, detail=detail)

    def _send(self, session, event, **fields):
        if session.cancelled:
            return
        self.write({"event": event, "session": session.id, **fields})


def has_signal(samples):
    """At least 0.3 s of audio whose loudest 20 ms frame is above a silent room's noise."""
    whole = len(samples) // FRAME * FRAME
    if whole < int(0.3 * SAMPLE_RATE):
        return False
    frames = np.asarray(samples[:whole], dtype=np.float32).reshape(-1, FRAME)
    return float(np.sqrt(np.mean(frames * frames, axis=1)).max()) >= FALLBACK_FLOOR


def load_model(spec):
    """The pinned snapshot from the local cache; never the network (serve sets the Hub offline)."""
    missing = missing_files(spec)
    if missing:
        raise ModelMissing(f"{spec.get('label')} is not downloaded ({len(missing)} files missing).")
    from mlx_audio.stt.utils import load
    # Nemotron is verified strict by riTranscribe; the others keep mlx-audio's own default.
    return load(spec["repo"], revision=spec["revision"], strict=spec.get("family") == "nemotron")


def transcribe(model, family, samples):
    import mlx.core as mx
    samples = np.asarray(samples, dtype=np.float32)
    try:
        if family == "parakeet":
            result = model.generate(mx.array(samples))
        elif family == "whisper":
            result = model.generate(samples, temperature=0.0, condition_on_previous_text=False)
        else:
            result = model.generate(samples, max_tokens=448, temperature=0.0)
        return (getattr(result, "text", "") or "").strip()
    finally:
        mx.clear_cache()


def hub_cache():
    from huggingface_hub import constants
    return Path(constants.HF_HUB_CACHE)


def snapshot_dir(spec):
    return hub_cache() / f"models--{spec['repo'].replace('/', '--')}" / "snapshots" / spec["revision"]


def missing_files(spec):
    root = snapshot_dir(spec)
    missing = []
    for name, size in spec.get("files", []):
        path = root / name
        try:
            if path.stat().st_size != size:
                missing.append(name)
        except OSError:
            missing.append(name)
    return missing


def download(spec):
    if not spec.get("downloadable", True):
        raise SystemExit(f"{spec.get('label')} can no longer be downloaded.")
    from huggingface_hub import snapshot_download
    names = [name for name, _ in spec["files"]]
    snapshot_download(spec["repo"], revision=spec["revision"], allow_patterns=names)
    missing = missing_files(spec)
    if missing:
        raise SystemExit(f"The download is incomplete: {', '.join(missing[:3])}")
    print(json.dumps({"event": "downloaded", "model": spec["key"]}), flush=True)


def read_wav(path):
    with wave.open(str(path), "rb") as file:
        if file.getnchannels() != 1 or file.getsampwidth() != 2 or file.getframerate() != SAMPLE_RATE:
            raise SystemExit("Expected a 16-bit mono 16 kHz WAV file.")
        data = np.frombuffer(file.readframes(file.getnframes()), dtype="<i2")
    return data.astype(np.float32) / 32768


def transcribe_file(spec, path, write):
    """Feeds a file through a dictation session in 100 ms packets, as the microphone would."""
    done = threading.Event()

    def record(event):
        write(event)
        if event["event"] in ("finished", "failed"):
            done.set()

    worker = Worker(record)
    worker.handle({"op": "begin", "session": 1, "model": spec})
    samples = read_wav(path)
    for start in range(0, len(samples), SAMPLE_RATE // 10):
        packet = samples[start:start + SAMPLE_RATE // 10].astype("<f4").tobytes()
        worker.handle({"op": "audio", "session": 1, "data": base64.b64encode(packet).decode()})
    worker.handle({"op": "finish", "session": 1})
    done.wait()
    worker.close()


def protocol_writer():
    """Protocol lines go to the original stdout; anything a library prints goes to stderr instead."""
    stream = os.fdopen(os.dup(1), "w", buffering=1, encoding="utf-8")
    os.dup2(2, 1)
    sys.stdout = sys.stderr
    lock = threading.Lock()

    def write(event):
        line = json.dumps(event, ensure_ascii=False)
        with lock:
            stream.write(line + "\n")
            stream.flush()
    return write


def serve():
    os.environ["HF_HUB_OFFLINE"] = "1"
    write = protocol_writer()
    worker = Worker(write)
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except ValueError:
            print(f"riwork-speech-worker: unreadable request", file=sys.stderr, flush=True)
            continue
        if isinstance(message, dict):
            worker.handle(message)
    worker.close()


def watch_parent():
    """Ends this process when RiWork does, even when it crashed: a download has no stdin to close."""
    parent = os.getppid()

    def watch():
        while True:
            time.sleep(1)
            if os.getppid() != parent:
                os._exit(0)
    threading.Thread(target=watch, name="parent-watch", daemon=True).start()


def main(argv):
    watch_parent()
    os.environ.setdefault("HF_HUB_DISABLE_TELEMETRY", "1")
    os.environ.setdefault("HF_HUB_DISABLE_PROGRESS_BARS", "1")
    command = argv[1] if len(argv) > 1 else ""
    if command == "serve":
        serve()
    elif command == "download" and len(argv) == 3:
        download(json.loads(argv[2]))
    elif command == "transcribe" and len(argv) == 4:
        os.environ["HF_HUB_OFFLINE"] = "1"
        write = protocol_writer()
        transcribe_file(json.loads(argv[2]), argv[3], write)
    else:
        raise SystemExit("usage: worker.py serve | download MODEL_JSON | transcribe MODEL_JSON FILE")


if __name__ == "__main__":
    main(sys.argv)
