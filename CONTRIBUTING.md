# Contributing to Grammalocal

Thank you for considering a contribution. Bug reports, feature ideas, documentation fixes and code are all welcome.

## Before you start

- For larger changes, open an issue first so the approach can be agreed before you write code.
- Read [docs/DESIGN.md](docs/DESIGN.md) for the architecture and the reasons behind it.
- Be kind and follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Development setup

You need:

- Rust stable (with `rustfmt` and `clippy`).
- Python 3.8 or newer.
- GTK 4 development files, for example `libgtk-4-dev` on Ubuntu or Debian.

```sh
git clone https://github.com/jere344/grammalocal.git
cd grammalocal
cargo build --workspace
```

## Checks

Run these before opening a pull request. CI runs the same checks.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
sh -n install.sh
```

The shell scripts must also parse under `dash`, since `install.sh` is run with `sh`:

```sh
dash -n install.sh
dash -n packaging/package.sh
```

## Code guidelines

- Keep changes focused. One logical change per pull request.
- Add or update tests when behaviour changes. Offset handling in particular needs tests.
- Keep `install.sh` POSIX `sh` compatible. Do not use bashisms.
- Do not add a dependency unless it is clearly needed, and explain why in the pull request.
- Write commit messages in the imperative mood, for example `Fix offset mapping for emoji`.

## Pull requests

1. Fork the repository and create a branch from `main`.
2. Make your change and run the checks above.
3. Update `CHANGELOG.md` under an `Unreleased` heading for user-visible changes.
4. Open a pull request and fill in the template.

A maintainer will review the pull request. Please be patient, and expect requests for changes.

## Licence

By contributing, you agree that your contributions are licensed under the GNU General Public License v3.0 or later, the same licence as the project.
