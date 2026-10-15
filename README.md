<p align="center">
  <img src="assets/logo-wordmark.svg" alt="grammalocal" width="520">
</p>

<p align="center">
  <b>A private, lightweight French &amp; English grammar checker for Linux — a local replacement for LanguageTool.</b>
</p>

<p align="center">
  <a href="https://github.com/jere344/grammalocal/actions/workflows/ci.yml"><img src="https://github.com/jere344/grammalocal/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/jere344/grammalocal/releases/latest"><img src="https://img.shields.io/github/v/release/jere344/grammalocal" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0-blue" alt="License: GPL-3.0"></a>
  <img src="https://img.shields.io/badge/platform-Linux-informational" alt="Platform: Linux">
</p>

---

grammalocal checks your spelling and grammar **on your own machine**. Nothing you type is sent to a server, it starts in a second, and it uses about **200 MB of RAM** — a fraction of a self-hosted LanguageTool server.

- ✍️ **Everywhere** — select text in any app, press **Super+C**, fix it, and paste the correction back in one click.
- 🌐 **In the browser** — the official LanguageTool extension works as-is, pointed at your local server.
- 🇫🇷 🇬🇧 **French and English, mixed** — the language is detected per sentence and per quotation.
- 🔒 **Private** — the server only listens on `127.0.0.1`.

## Contents

- [Why](#why)
- [Quick start](#quick-start)
- [Usage](#usage)
- [Configuration](#configuration)
- [Command line](#command-line)
- [How it works](#how-it-works)
- [Building from source](#building-from-source)
- [Uninstall](#uninstall)
- [Limitations](#limitations)
- [FAQ](#faq)
- [Contributing](#contributing) · [License & credits](#license--credits)

## Why

|                         | grammalocal                         | LanguageTool server (self-hosted) |
| ----------------------- | ----------------------------------- | --------------------------------- |
| RAM                     | ~200 MB                             | ~0.5–1+ GB (Java)                 |
| Speed                   | ~10 ms per paragraph                | slower, JVM warm-up               |
| Privacy                 | local only                          | local only                        |
| French grammar          | Grammalecte (excellent)             | good                              |
| English grammar         | Harper (good, some gaps)            | very good                         |
| Mixed FR/EN in one text | ✅ per sentence & quotation          | one language per request          |
| Works outside browser   | ✅ hotkey popup in every app         | ❌                                 |
| Setup                   | one command                         | Java + Docker/manual              |

What makes it pleasant to use day to day:

- **Mixed text works.** `Il a dit « this are wrong » et les enfant mange.` — the quote is checked in English, the rest in French.
- **Franglais isn't flagged.** "On a fait un meeting avec le manager" produces no spelling errors.
- **No noise.** Emojis are never flagged, and typography rules nobody follows when typing (curly apostrophes and quotes, non-breaking spaces before `! ? : ;`) are turned off.

## Quick start

**Requirements:** Linux x86_64 with a systemd user session, Python ≥ 3.8 and GTK ≥ 4.14 (Ubuntu 24.04+, Fedora 40+, Arch, …). The popup's shortcut and automatic paste are designed for GNOME on Wayland.

### 1. Install

```sh
curl -fsSL https://raw.githubusercontent.com/jere344/grammalocal/main/install.sh | sh
```

No root needed: everything goes to `~/.local`. The script downloads the latest release, verifies its SHA-256 checksum, and runs `grammalocal setup`, which:

1. checks prerequisites,
2. starts the `grammalocald` background service (and at every login),
3. binds **Super+C** to the popup (GNOME),
4. optionally enables automatic paste,
5. prints the browser setup steps below.

Add `-s -- --yes` after `sh` to accept every default without questions.

### 2. Browser

1. Install the LanguageTool extension for [Firefox](https://addons.mozilla.org/firefox/addon/languagetool/) or [Chrome](https://chromewebstore.google.com/detail/oldceeleldhonbafppcapldpdifcinji).
2. Open its settings → **Advanced settings** → **LanguageTool server** → **Other server**.
3. Enter `http://localhost:8091/v2`.

That's it — errors are now underlined as you type, checked locally.

### 3. Any other app

Select text anywhere (editor, chat app, notes…) and press **Super+C**.

## Usage

### The popup

The selected text opens in a small window with errors underlined (red for spelling, blue for grammar).

| Action                     | How                                    |
| -------------------------- | -------------------------------------- |
| See suggestions            | click an underlined word, or **Tab** / **Shift+Tab** |
| Apply a suggestion         | click it                               |
| Fix everything at once     | **Accept all** (first suggestion each) |
| Edit by hand               | just type — it re-checks after 0.5 s   |
| Force a language           | Auto / Français / English menu         |
| Apply                      | **Apply** button, **Enter** (Ctrl+Enter while editing) |
| Cancel                     | **Esc**                                |

**Apply** has two modes:

- `copy` (default) — the corrected text goes to the clipboard; press Ctrl+V.
- `replace` — the corrected text is pasted over your selection automatically. This uses GNOME's remote-desktop portal, which asks for permission once. Enable it with `grammalocal-popup --setup-replace` (or during `grammalocal setup`); go back with `grammalocal-popup --setup-copy`.

### The browser

Use the LanguageTool extension exactly as before. "Add to dictionary" adds the word to your personal dictionary in `config.toml`.

## Configuration

`~/.config/grammalocal/config.toml` is created on first run. Restart the service after editing it: `systemctl --user restart grammalocald`.

| Key                        | Default  | Description |
| -------------------------- | -------- | ----------- |
| `port`                     | `8091`   | Local HTTP port (always bound to `127.0.0.1`). |
| `default_language`         | `"fr"`   | Language used when detection is undecided (`fr` or `en`). |
| `apply_mode`               | `"copy"` | Popup Apply behaviour: `copy` or `replace`. |
| `personal_dictionary`      | `[]`     | Words never reported as spelling errors. |
| `disabled_rules`           | `[]`     | Rule ids (or id prefixes) to ignore, e.g. `"harper.Spelling"`. Rule ids are shown by `grammalocal check`. |
| `allow_browser_extensions` | `true`   | Accept requests from any browser extension without a token. Web pages cannot impersonate an extension. |
| `allowed_origins`          | `[]`     | Extra origins allowed without a token (use if you set the option above to `false`). |
| `token`                    | random   | Bearer token for other local clients (`Authorization: Bearer …`). Generated on first run; the file is `0600`. |
| `log_requests`             | `false`  | Log one line per request (method, path, sizes — never the text) to the journal. |

## Command line

```text
grammalocal setup [--yes]          set up service, shortcut, paste mode; print browser steps
grammalocal status                 service state, memory, config path, shortcut
grammalocal check [--lang auto|fr|en] [--server] [TEXT]
                                   check TEXT (or stdin) and print errors with rule ids
grammalocal uninstall [--purge]    remove everything (--purge also deletes the config)
grammalocal-popup [--setup-replace | --setup-copy]
```

Logs: `journalctl --user -u grammalocald -f`

## How it works

```text
 grammalocal-popup (GTK4) ──┐                              ┌─► Harper (Rust, in-process) ── English
                            ├─► grammalocald  127.0.0.1 ───┤
 LanguageTool extension ────┘   LanguageTool-compatible    └─► Grammalecte (Python worker) ─ French
                                /v2/check API
```

For every request, `grammalocald`:

1. splits the text into paragraphs, sentences and quotations;
2. detects each part's language (restricted to French and English); when detection is unsure on short text, it checks with both engines and keeps the one that finds fewer problems;
3. sends each part to its engine — a quotation in another language is masked in the surrounding sentence and checked separately;
4. drops French spelling errors on valid English words (and vice versa);
5. converts offsets to UTF-16 (what browsers use) and answers in the LanguageTool format.

Results are cached per paragraph, so the extension's re-checks while you type are nearly free. The Grammalecte worker is restarted in the background if it ever grows past 150 MB while idle.

## Building from source

Needs Rust ≥ 1.89 and GTK 4 development files (`libgtk-4-dev` on Debian/Ubuntu, `gtk4-devel` on Fedora, `gtk4` on Arch).

```sh
git clone https://github.com/jere344/grammalocal.git
cd grammalocal
./install.sh          # builds in release mode, installs to ~/.local, runs setup
```

Development: `cargo test --workspace`, `cargo clippy --workspace --all-targets`, `cargo fmt`. See [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/DESIGN.md](docs/DESIGN.md).

## Uninstall

```sh
grammalocal uninstall           # keeps ~/.config/grammalocal
grammalocal uninstall --purge   # removes the config too
```

Remove the custom server URL from the LanguageTool extension afterwards.

## Limitations

- **English coverage depends on [Harper](https://github.com/Automattic/harper)**, which still misses some errors (e.g. "This are", "your/you're"). It improves with every release.
- Prebuilt binaries are **x86_64 Linux** only; other architectures can build from source.
- The Super+C shortcut is registered automatically on **GNOME** only; on other desktops bind `grammalocal-popup` to a key yourself. Automatic paste needs the XDG RemoteDesktop portal (GNOME).
- The LanguageTool extension is a **third-party proprietary** extension; its "Other server" option could change.

## FAQ

**Is my text sent anywhere?**
No. grammalocal only listens on `127.0.0.1` and makes no network requests. The LanguageTool extension is not part of grammalocal, so what it does on its own is up to it.

**Does it work on KDE / X11?**
The service, CLI and browser part work anywhere with systemd. The popup works on any desktop; bind `grammalocal-popup` to a shortcut yourself. Automatic paste depends on your desktop's RemoteDesktop portal support.

**Other languages?**
Not yet — French and English only. Requests for other languages return no errors instead of failing.

**How do I stop a rule from being reported?**
Run `grammalocal check "your text"` to see the rule id, then add it to `disabled_rules`.

## Contributing

Issues and pull requests are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Grammar rules themselves live upstream in [Grammalecte](https://grammalecte.net/) and [Harper](https://github.com/Automattic/harper).

## License & credits

grammalocal is licensed under the [GNU GPL v3.0 or later](LICENSE).

It stands on the shoulders of:

- [Grammalecte](https://grammalecte.net/) by Olivier R. — French grammar checker (GPL-3.0), bundled in `vendor/`.
- [Harper](https://github.com/Automattic/harper) by Automattic — English grammar checker (Apache-2.0).
- [LanguageTool](https://languagetool.org/) — grammalocal implements a compatible API; it is not affiliated with or endorsed by LanguageTool.
