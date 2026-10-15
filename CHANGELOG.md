# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-10-15

First public release.

### Added

- `grammalocald` daemon with a LanguageTool-compatible HTTP API on `127.0.0.1:8091`.
- French checking with the bundled Grammalecte v2.3.0 worker, and English checking with Harper.
- Per-paragraph language detection, franglais filtering, and UTF-16 offsets in API responses.
- Personal dictionary, disabled rules, excluded sites and bearer token authentication.
- `grammalocal-popup`, a GTK 4 popup for checking the current selection, bound to `Super+C` on GNOME.
- `grammalocal` command line tool with `check`, `setup`, `status` and `uninstall` subcommands, and `--version`.
- systemd user service for the daemon.
- `install.sh` for release tarballs, source checkouts, and piped installation with SHA-256 verification.
- Application icon and desktop entry, installed under the user's XDG data directory.
- Release packaging script `packaging/package.sh` and GitHub Actions workflows for CI and releases.

[0.1.0]: https://github.com/jere344/grammalocal/releases/tag/v0.1.0
