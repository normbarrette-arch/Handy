# PERSONAL.md — fork build & auto-update runbook

This file is **specific to the `normbarrette-arch/Handy` fork** and the
`personal` branch. It is not relevant upstream. It documents the private
build pipeline, the signing key, the version scheme, and the non-obvious
gotchas hit while setting this up so a future session (or you) doesn't
re-learn them.

## What the fork adds on top of upstream

Branch `personal` carries, on top of `cjpais/Handy`:

1. **Focus-restore fix (#315)** — `src-tauri/src/focus.rs`. Captures the
   foreground window on hotkey-down, restores it before keystroke
   synthesis. Windows (`SetForegroundWindow` + `AttachThreadInput`, retry
   once, emits `focus-restore-failed` → toast) and Linux X11 (cached
   `x11rb` connection, EWMH `_NET_ACTIVE_WINDOW`). Wayland/macOS no-op.
2. **LLM post-processing perf (Phase 1)** — `llm_client.rs`,
   `actions.rs`: cached `reqwest::Client` per provider (warm TLS),
   20s/5s timeouts, `temperature: 0` + `max_tokens` cap, dropped the
   JSON-schema wrapper (plain system+user). Default prompt tightened
   (~40% fewer tokens) — only affects fresh installs, not stored prompts.
3. **Reliability (Phase 2)** — enigo lock acquired before focus restore;
   Linux paste-tool probes cached; shortcut rebind **rolls back** on
   registration failure (kills the "stuck shortcut, needs restart" bug)
   with a clear conflict message.
4. **LLM retry (Phase 3c)** — single retry on 429/5xx/connect/timeout,
   fail-fast on 4xx.
5. **In-app updater fixes (Phase 3a + bugfix)** — NSIS (not MSI),
   per-user, `installMode: passive`; updater errors surface as a toast;
   `log_frontend` command bridges webview diagnostics into `handy.log`;
   the actual fix was wrapping `isPortable()` inside the `installUpdate`
   try/catch (an unhandled rejection there = "click does nothing").
6. **CI (`.github/workflows/personal-build.yml`)** — see below.

Deferred by deliberate scope call (low ROI vs regression risk given the
timeout+retry safety nets): in-flight LLM cancellation token; audio
mutex-poison sweep; thread-spawn refactor. Phase 4 (streaming opt-in +
adaptive paste delay) not done.

## Build & distribution pipeline

- Push to `personal` → `.github/workflows/personal-build.yml` runs on a
  `windows-latest` runner → builds a **signed NSIS `-setup.exe`** +
  `.sig` + `latest.json`, publishes to the rolling GitHub Release tagged
  **`personal-latest`**.
- Both PCs auto-update: tray → **Check for updates** → main-window
  footer → **Update available** → downloads, installs (passive, no
  UAC), relaunches (~1 s). No manual steps.
- Manual download (only ever needed to cross a key rotation or the
  one-time MSI→NSIS switch — neither should recur):
  `https://github.com/normbarrette-arch/Handy/releases/tag/personal-latest`

### Version scheme

`personal-build.yml` rewrites `tauri.conf.json` version to
`0.8.<100 + GITHUB_RUN_NUMBER>` per run, so every build is strictly
newer and the updater always sees an upgrade. Do not hand-set the
version on this branch.

### Signing key — CRITICAL, back this up

- Updater signature uses a minisign keypair. **Private key + password
  live only on the primary PC:**
  - `C:\Users\normb\.tauri\handy_updater.key`
  - `C:\Users\normb\.tauri\handy_updater.password`
- Public key is embedded in `src-tauri/tauri.conf.json`
  (`plugins.updater.pubkey`).
- GitHub secrets on the fork: `TAURI_SIGNING_PRIVATE_KEY`,
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
- **If both local files are lost, no installed copy can ever
  auto-update again** (you'd rotate the key → one-time manual reinstall
  on every machine). Keep them in a password manager / offline backup.
- **Do not rotate the key.** Rotation breaks auto-update for every
  already-installed copy until a manual reinstall. The current key is
  final.

## Gotchas (learned the hard way)

- **Empty commits don't trigger CI.** The workflow has
  `paths-ignore: "**/*.md"`; a `--allow-empty` commit changes no paths
  and GitHub skips the run. To force a build with no code change, use
  `gh workflow run "Personal Windows Build" --ref personal`
  (workflow_dispatch still increments `run_number`).
- **A pure `.md` commit will NOT build** (same `paths-ignore`) — fine
  for docs, but don't expect an auto-update from a docs-only push.
- **NSIS, never MSI**, for Windows auto-update. MSI updates call
  `msiexec` which needs UAC elevation; the tauri updater stalled
  post-download with the error swallowed to the webview console. NSIS
  per-user + `installMode: passive` is the supported path.
- **Password secret must be newline-free.** Setting it from a file
  written by `echo` stored `<pw>\n`, which mismatched the key's actual
  password ("Wrong password for that key" *after* a clean compile). Use
  `printf '%s' "$PW" | gh secret set ...`.
- **`gh repo fork <repo> --remote` is rejected** when a repo arg is
  given; use `gh repo fork <repo> --clone=false` then
  `git remote add origin ...`.
- **Release assets carry the version in the filename**, so the workflow
  prunes non-current assets *after* a successful publish (never before —
  pre-publish purge left the release empty mid-build).
- `tauri-action` sets the release **name only on creation**; the
  workflow runs `gh release edit --title` each build so the rolling
  release name doesn't go stale.
- **`webview console errors never reach `handy.log`.** The
  `log_frontend` Tauri command + `ulog()` in `UpdateChecker.tsx` exist
  precisely so updater failures are remotely diagnosable. Keep them.

## Verifying a build

```
# from repo root
cd src-tauri && cargo fmt && cargo check && cargo clippy --no-deps
cd .. && bun run lint && bunx tsc --noEmit -p tsconfig.json
```

`handy.log` is at
`%LOCALAPPDATA%\com.pais.handy\logs\handy.log`. Useful greps:
`[frontend] installUpdate` (updater trace), `focus: restored`,
`Starting LLM post-processing`, `LLM post-processing succeeded`.

## Upstreaming

The focus-restore fix (#315) is the only piece worth a PR to
`cjpais/Handy`. Everything else (CI, signing, version scheme) is
fork-private. Use `gsd-pr-branch`-style filtering to exclude the
`personal-build.yml` / version-bump churn if opening that PR.
