# Text to speech

James speaks with [Piper](https://github.com/OHF-Voice/piper1-gpl), locally on the server. Speech
plays over the music, which is lowered while the bot talks, and several texts are queued.

- Chat: `!say Hello everyone` (`!hush` stops)
- Web interface: **Text to speech** on the instance page
- API: `POST /api/v1/instances/<id>/say` with `{"text": "…", "language": "en"}` (`202` with the
  speech id; `speechStarted` / `speechEnded` events follow)
- Plugins: `bot.say('…')` / SinusBot `audio.say('…', 'en')`

Every instance can have its own voice (**Settings → Voice**); otherwise `tts.default_voice` is
used. A requested language (`"de"`, `"en-US"`) picks the best installed voice for it.

## Voices

The Docker image contains `de_DE-thorsten-medium` and `en_US-lessac-medium`. More voices (over 30
languages): [list and samples](https://rhasspy.github.io/piper-samples/). To add one, put the
`.onnx` and `.onnx.json` files into the voices directory and restart James:

```sh
# Docker: mount a directory with voices and point James to it
docker compose exec james /opt/piper/bin/python -m piper.download_voices \
    --download-dir /data/voices fr_FR-siwis-medium de_DE-thorsten-medium en_US-lessac-medium
# then set JAMES_TTS__PIPER__VOICES_DIR=/data/voices and restart
```

Each voice in use keeps its model loaded (about 100–200 MB of memory), at most
`tts.piper.max_workers` at a time; unused voices are unloaded after `idle_timeout_secs` (the default
voice stays loaded). A sentence takes a fraction of a second to synthesize once the voice is loaded.
Texts are limited to 500 characters and 16 per instance may wait. If a voice fails to load (see the
log), requests for it fail for a minute before James tries again.

Throughput: each voice has one worker, so texts with the same voice are synthesized one after the
other (across all instances), and at most `max_workers` texts are synthesized at once. That is
plenty for chat use; with many busy instances, give them different voices or raise `max_workers`.
Speech follows the instance volume (at volume 0 the bot is silent).

## Other providers

Text to speech is behind a provider interface (`TtsProvider` in `james-core`), so other engines
(cloud services, other local models) can be added without changing the rest of James.
