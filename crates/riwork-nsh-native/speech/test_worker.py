"""The speech worker's dictation rules, with a fake model: no MLX, no downloads.

Run with the speech runtime's Python (it needs numpy):  python -I -m unittest discover -s crates/riwork-nsh-native/speech
"""
import base64
import threading
import unittest

import numpy as np

import worker as w

RATE = w.SAMPLE_RATE
WORDS = {1: "alpha", 2: "beta", 3: "gamma"}


def tone(level, seconds):
    """Speech stand-in: a constant-amplitude signal whose level names the word the fake model hears."""
    return np.full(int(seconds * RATE), level / 10, dtype=np.float32)


def silence(seconds):
    return np.zeros(int(seconds * RATE), dtype=np.float32)


def fake_transcriber(model, family, samples):
    level = int(round(float(np.max(np.abs(samples))) * 10))
    return WORDS.get(level, "quiet" if level == 0 else f"level{level}")


class Recorder:
    def __init__(self):
        self.events = []
        self.changed = threading.Condition()

    def __call__(self, event):
        with self.changed:
            self.events.append(event)
            self.changed.notify_all()

    def wait_for(self, kind, session=1, timeout=5):
        with self.changed:
            found = self.changed.wait_for(
                lambda: any(e["event"] == kind and e["session"] == session for e in self.events), timeout)
        if not found:
            raise AssertionError(f"no {kind} for session {session}: {self.events}")
        return [e for e in self.events if e["event"] == kind and e["session"] == session][-1]

    def kinds(self, session=1):
        return [e["event"] for e in self.events if e.get("session") == session]


SPEC = {"key": "fake", "label": "Fake model", "family": "parakeet", "draft_seconds": 0.5}


def audio(samples):
    return base64.b64encode(np.asarray(samples, dtype="<f4").tobytes()).decode()


class SegmenterTest(unittest.TestCase):
    def collect(self, *parts, **options):
        chunks = []
        segmenter = w.Segmenter(chunks.append, **options)
        for part in parts:
            for start in range(0, len(part), 1000):  # odd packet sizes cross frame boundaries
                segmenter.feed(part[start:start + 1000])
        return segmenter, chunks

    def test_silence_emits_nothing(self):
        segmenter, chunks = self.collect(silence(3))
        segmenter.flush()
        self.assertEqual(chunks, [])

    def test_a_pause_ends_a_phrase_and_drafts_grow_it(self):
        _, chunks = self.collect(tone(1, 2.0), silence(1.0), tone(2, 0.6), silence(1.0), draft_seconds=0.5)
        finals = [c for c in chunks if c.final]
        drafts = [c for c in chunks if not c.final]
        self.assertEqual([c.utterance for c in finals], [1, 2])
        self.assertGreaterEqual(len([d for d in drafts if d.utterance == 1]), 3)
        revisions = [d.revision for d in drafts if d.utterance == 1]
        self.assertEqual(revisions, sorted(revisions))
        # The pre-roll and silence tail are part of the phrase, so it is longer than the tone.
        self.assertGreater(len(finals[0].samples), 2.0 * RATE)

    def test_a_long_phrase_is_cut_at_the_limit(self):
        _, chunks = self.collect(tone(1, 5), max_seconds=2.0, draft_seconds=10)
        self.assertEqual([c.utterance for c in chunks if c.final], [1, 2])

    def test_stopping_flushes_the_phrase_in_progress(self):
        segmenter, chunks = self.collect(tone(1, 0.5))
        self.assertEqual(chunks, [])
        segmenter.flush(final=True)
        self.assertEqual([c.final for c in chunks], [True])

    def test_a_click_is_not_speech(self):
        segmenter, chunks = self.collect(tone(1, 0.06), silence(1))
        segmenter.flush()
        self.assertEqual(chunks, [])


class WorkerTest(unittest.TestCase):
    def setUp(self):
        self.out = Recorder()
        self.loads = []

        def loader(spec):
            if spec.get("missing"):
                raise w.ModelMissing(f"{spec['label']} is not downloaded.")
            self.loads.append(spec["key"])
            return object()

        self.worker = w.Worker(self.out, loader=loader, transcriber=fake_transcriber)

    def tearDown(self):
        self.worker.close()

    def send(self, op, session=1, **fields):
        self.worker.handle({"op": op, "session": session, **fields})

    def test_dictation_reports_cumulative_text_and_a_final(self):
        self.send("begin", model=SPEC)
        self.out.wait_for("loaded")
        self.assertEqual(self.out.kinds()[:2], ["note", "loaded"])
        self.send("audio", data=audio(tone(1, 1.2)))
        self.out.wait_for("heard")
        self.send("audio", data=audio(np.concatenate((silence(1), tone(2, 0.6)))))
        self.send("finish")
        finished = self.out.wait_for("finished")
        self.assertEqual(finished["text"], "alpha beta")
        heard = [e["text"] for e in self.out.events if e["event"] == "heard"]
        self.assertTrue(all(text.startswith("alpha") for text in heard), heard)
        self.assertEqual(self.loads, ["fake"])

    def test_a_loaded_model_is_reused_and_a_new_one_replaces_it(self):
        self.send("begin", model=SPEC)
        self.out.wait_for("loaded")
        self.send("finish")
        self.out.wait_for("finished")
        self.send("begin", session=2, model=SPEC)
        self.out.wait_for("loaded", session=2)
        self.assertEqual(self.out.kinds(2), ["loaded"])  # no second load, no note
        self.send("cancel", session=2)
        self.send("begin", session=3, model={**SPEC, "key": "other", "label": "Other"})
        self.out.wait_for("loaded", session=3)
        self.assertEqual(self.loads, ["fake", "other"])

    def test_a_missing_model_fails_the_dictation(self):
        self.send("begin", model={**SPEC, "missing": True})
        failed = self.out.wait_for("failed")
        self.assertEqual(failed["problem"], "model-unavailable")
        self.send("audio", data=audio(tone(1, 1)))
        self.send("finish")
        self.assertNotIn("finished", self.out.kinds())

    def test_a_quiet_microphone_is_still_transcribed_at_the_end(self):
        self.send("begin", model=SPEC)
        self.send("audio", data=audio(np.full(RATE, 0.003, dtype=np.float32)))  # below the gate
        self.send("finish")
        self.assertEqual(self.out.wait_for("finished")["text"], "quiet")

    def test_silence_is_not_sent_to_the_model(self):
        decoded = []
        worker = w.Worker(self.out, loader=lambda spec: object(),
                          transcriber=lambda *args: decoded.append(1) or "thank you", warm_up=False)
        try:
            worker.handle({"op": "begin", "session": 7, "model": SPEC})
            worker.handle({"op": "audio", "session": 7, "data": audio(np.full(2 * RATE, 0.0004, dtype=np.float32))})
            worker.handle({"op": "finish", "session": 7})
            self.assertEqual(self.out.wait_for("finished", session=7)["text"], "")
            self.assertEqual(decoded, [])
        finally:
            worker.close()

    def test_nothing_is_reported_after_cancel(self):
        self.send("begin", model=SPEC)
        self.out.wait_for("loaded")
        self.send("audio", data=audio(tone(1, 1.2)))
        self.send("cancel")
        self.send("finish")
        self.worker.handle({"op": "unload"})
        before = list(self.out.events)
        threading.Event().wait(0.3)
        self.assertEqual([e for e in self.out.events if e not in before], [])
        self.assertNotIn("finished", self.out.kinds())

    def test_an_idle_model_is_unloaded(self):
        now = [0.0]
        worker = w.Worker(self.out, loader=lambda spec: object(), transcriber=fake_transcriber,
                          idle_seconds=5, clock=lambda: now[0])
        try:
            worker.handle({"op": "begin", "session": 9, "model": SPEC})
            self.out.wait_for("loaded", session=9)
            worker.handle({"op": "finish", "session": 9})
            self.out.wait_for("finished", session=9)
            self.assertIsNotNone(worker.model)
            now[0] = 6
            for _ in range(50):
                if worker.model is None:
                    break
                threading.Event().wait(0.05)
            self.assertIsNone(worker.model)
        finally:
            worker.close()


if __name__ == "__main__":
    unittest.main()
