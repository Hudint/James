# Whisper worker of James: keeps one faster-whisper model loaded and transcribes requests read as
# JSON lines from stdin ({"id", "audio": base64 16 kHz mono s16le, "language", "prompt"}),
# answering with JSON lines ({"id", "text", "language"} or {"id", "error"}).
import base64
import json
import sys

# Answers only: anything libraries print goes to stderr.
answers = sys.stdout
sys.stdout = sys.stderr

import numpy
from faster_whisper import WhisperModel

# What Whisper "hears" in noise or silence (subtitle credits of its training data).
HALLUCINATIONS = ("untertitel", "amara.org", "thanks for watching", "vielen dank für's zuschauen",
                  "vielen dank fürs zuschauen", "subtitles by")


def answer(value):
    answers.write(json.dumps(value) + "\n")
    answers.flush()


model = WhisperModel(sys.argv[1], device="cpu", compute_type="int8", cpu_threads=int(sys.argv[2]))
answer({"ready": True})
for line in sys.stdin:
    try:
        request = json.loads(line)
    except ValueError:
        # Never stay silent: the host would wait for an answer until its timeout.
        answer({"error": "unreadable request"})
        continue
    try:
        pcm = numpy.frombuffer(base64.b64decode(request["audio"]), dtype=numpy.int16)
        segments, info = model.transcribe(
            pcm.astype(numpy.float32) / 32768.0,
            language=request.get("language") or None,
            initial_prompt=request.get("prompt") or None,
            beam_size=1,
            condition_on_previous_text=False,
            without_timestamps=True,
            vad_filter=True,
        )
        text = " ".join(
            segment.text.strip()
            for segment in segments
            if segment.no_speech_prob < 0.6 and not any(h in segment.text.lower() for h in HALLUCINATIONS)
        ).strip()
        answer({"id": request["id"], "text": text, "language": info.language})
    except Exception as error:  # report, keep serving
        answer({"id": request.get("id"), "error": str(error)})
