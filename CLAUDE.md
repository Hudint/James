# James – working agreements

Project scope, architecture and milestones: see [PLAN.md](PLAN.md).

## Review before every commit

Before **every** commit, all review agents defined in `.claude/agents/` must review the pending
changes (`git diff` / new files):

- `code-reviewer` — always.
- `security-reviewer` — always.
- `ui-ux-reviewer` — whenever the web UI (`web/`) changed; give it fresh screenshots (desktop + mobile,
  light + dark).

Fix every valid finding (or document why it is not addressed) before committing. Reviewers run in
parallel with further work where possible.

## Conventions

- Rust: `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must pass.
- Live TeamSpeak tests: `JAMES_TS_TEST_ADDR=localhost:19987` (TS3) / `localhost:29987` (TS6) with the
  `james-test-ts3` / `james-test-ts6` Docker containers. Never touch the production TS server on the
  host (port 9987).
- Docs and UI text in English, UI translated to German.
- Every extension point (platform, source, TTS, STT, commands) goes behind a trait/registry.
