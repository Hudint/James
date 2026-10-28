# Piper worker of James: keeps one voice model loaded and synthesizes requests read as JSON
# lines from stdin ({"id", "text", "output"}) into WAV files, answering with JSON lines.
import json
import sys
import wave

from piper import PiperVoice

voice = PiperVoice.load(sys.argv[1])
print(json.dumps({"ready": True}), flush=True)
for line in sys.stdin:
    try:
        request = json.loads(line)
    except ValueError:
        # Never stay silent: the host would wait for an answer until its timeout.
        print(json.dumps({"error": "unreadable request"}), flush=True)
        continue
    try:
        with wave.open(request["output"], "wb") as output:
            voice.synthesize_wav(str(request["text"]), output)
        print(json.dumps({"id": request["id"], "ok": True}), flush=True)
    except Exception as error:  # report, keep serving
        print(json.dumps({"id": request.get("id"), "error": str(error)}), flush=True)
