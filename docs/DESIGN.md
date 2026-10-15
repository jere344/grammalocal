# Grammalocal design

Self-hosted FR/EN grammar & spelling checker for Linux (GNOME/Wayland), replacing LanguageTool.

## Goals
- Grammar + spelling for French and English, automatic language detection, mixed-language text.
- Low idle memory (~100 MB target); peak during checks ≤ 500 MB.
- Fast: paragraph check ≈ 10–20 ms.
- Two clients sharing one backend: a hotkey popup (works in every app) and a browser extension (Firefox first, Chrome later).
- Later: Element extension using the same backend.

## Architecture

```
 hotkey popup (GTK4) ──┐                         ┌─ Harper (harper-core, in-process) → EN
                       ├─► grammalocald (Rust) ────┤
 browser extension ────┘   127.0.0.1 HTTP        └─ Grammalecte worker (Python, bundled) → FR
                           systemd user service
```

## 1. `grammalocald` (Rust daemon)

### API
LanguageTool-compatible subset so existing LT clients work:
- `POST /v2/check` — form fields `text`, `language` (`auto` | `fr` | `en`). Returns `{ language, matches: [{ offset, length, message, shortMessage, replacements: [{value}], rule: {id, category}, context }] }`.
- `GET /v2/languages`.
- Offsets/lengths are in **UTF-16 code units** (what browsers and LT clients expect).

### Pipeline
1. **Segment**: paragraphs → sentences → quoted spans (`« »`, `" "`, `“ ”`).
2. **Detect language** per segment with `whatlang`, restricted to FR/EN.
   - Low confidence (short text): run both engines; pick the one with no errors; if both have errors, the one with fewer spelling errors; tie → paragraph language → configured default.
3. **Dispatch**: EN → Harper; FR → Grammalecte (embedded quotes of another language replaced by a neutral placeholder of equal length).
4. **Franglais filter**: drop a FR spelling error if the word is valid in Harper's English dictionary (and vice versa).
5. **Offset mapping**: Rust bytes / Python code points / UTF-16 → all converted to UTF-16 on output. Dedicated tests.
6. **User filters**: personal dictionary, disabled rules.

### Cache
LRU keyed by (paragraph hash, language mode). The extension resends whole text on each debounce; only changed paragraphs are rechecked.

### Grammalecte worker
- Python child process, JSON lines over stdin/stdout (no Bottle server).
- Bundled Grammalecte at a pinned version (currently v2.3.0).
- Spell suggestions computed lazily (only for flagged words returned to the client).
- **Recycling**: after 30 s idle, if worker RSS > 150 MB, spawn a fresh worker in the background (~0.6 s load), swap, kill the old one. Fallback option if needed: fork-per-check from a pre-loaded parent.

### Security
- Binds `127.0.0.1` only.
- Requires `Authorization: Bearer <token>` (token in config file, generated on first run).
- CORS allowed only for the extension origin(s).

### Memory budget
Daemon + Harper ≈ 20–40 MB (to measure) + Grammalecte idle ≈ 70 MB → ~100 MB idle.

## 2. `grammalocal-popup` (Rust, GTK4)
- Launched by a GNOME custom shortcut (`Super+C`), or by running `grammalocal-popup`.
- Reads the Wayland PRIMARY selection once focused.
- Shows text with highlighted errors; click an error → suggestions; "Accept all"; FR/EN/Auto toggle.
- Apply (Enter) / close (Esc). Apply mode from config:
  - `copy` (default): corrected text → clipboard, user pastes.
  - `replace`: popup closes, focus returns with selection intact, popup sends Ctrl+V via the RemoteDesktop portal (one-time permission) or `ydotool` fallback.

## 3. Browser extension (planned; not in this repository yet)
- Content script on `textarea` / `contenteditable`, 400 ms debounce → daemon.
- Underlines drawn on a mirrored overlay layer (hardest part; rich editors like Gmail/Jira need care).
- Click on an underline → custom suggestion popover (dynamic context-menu entries aren't feasible).
- Per-site enable/disable, add-to-dictionary.

## Configuration
`~/.config/grammalocal/config.toml`, shared by all components: default language, apply mode, personal dictionary, disabled rules, excluded sites, token, port.

## Roadmap
1. **Daemon core** + CLI (`grammalocal check "texte"`)
   - 1a. Cargo workspace, bundled Grammalecte worker, Rust worker client, FR check via CLI.
   - 1b. Harper integration, segmentation, language detection, franglais filter, UTF-16 offset mapping + tests.
   - 1c. HTTP API, cache, worker recycling, config/token, systemd unit.
2. **Popup** (done; needs `libgtk-4-dev` to build from source).
3. **Firefox extension**, then Chrome (planned).
4. **Element extension** (planned; spell-check via existing extension framework).

## Environment notes
- Tested on Ubuntu with GNOME on Wayland; Rust 1.87 and Python 3.12.
- Building the popup needs GTK4 development headers (`libgtk-4-dev`).
- Benchmarks: `bench/` (Grammalecte v2.3.0: load ~0.6 s, ~10 ms/paragraph, 71 MB idle after load, ~436 MB peak).
