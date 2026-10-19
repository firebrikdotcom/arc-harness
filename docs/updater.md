# Automatic harness updates

The Rust updater is a one-way distributor for Linux and macOS computers. Every
60 seconds it fetches `master` into its own bare cache. It activates the exact
branch head only after the latest push run of every configured workflow has
completed successfully. Pending, missing, failed and unreadable CI evidence
leave the current installation running. It never commits, merges, pushes,
stashes or changes the developer checkout. Phones and tablets are excluded.

## Install

From the developer checkout, after its published commit passes CI:

```sh
python3 scripts/install-updater.py
```

Requires Rust/Cargo to build the updater, Git, Python 3.10+, curl and tar. GitHub
CLI is optional: existing authentication is used when available, and public
GitHub workflow evidence can be read without creating credentials. Private
repository access requires separately configured authentication. No credentials
are copied or changed by this installer. A network/API/rate-limit failure keeps
the current version and records the error.

The installer creates a user systemd service on Linux or a LaunchAgent on macOS.
It preserves unrelated hooks, registers stable hook commands and installs:

- `~/.local/bin/harness`: the managed harness CLI.
- `~/.local/bin/harness-runtime`: updater control and other managed scripts.
- `~/.local/libexec/harness-updater`: the small updater executable.
- `~/.config/harness/updater.json`: non-secret repository, workflow and tool settings.
- `~/.local/share/harness-updater/`: cache, releases, pointers, locks and status.

`XDG_CONFIG_HOME` is respected for configuration. `--no-service` installs without
starting the background service. `--repository`, `--url`, `--branch`, repeated
`--workflow` and `--interval` configure an initial installation. Existing settings
must match when reinstalling; edit the settings explicitly before changing them.
The default required workflow is `ci.yml`. If more publication workflows become
required, add every one to `workflows`; unlisted workflows are not inferred.

The installed-root marker follows the active pointer so guard-version checks
see the current version. Unpublished local hook additions absent from a release
are preserved in place.

New agent sessions load the managed hook commands. Already running sessions
retain their loaded hooks and must be restarted to adopt those commands.

## Operation and state

Each version is extracted to a fresh release directory and smoke-checked before
an atomic `current` pointer switch. `previous` retains the last active version.
The Rust launcher holds a shared operation lock, resolves the pointer once,
runs that release and waits for its child to finish. Activation takes an
exclusive lock; busy operations defer it until a later poll. Multiple polling
processes serialize through a separate update lock.

Hooks and `harness` use this launcher. For another script, use:

```sh
harness-runtime exec scripts/verify.sh --project /absolute/project
```

Directly invoking scripts in the editable checkout is development execution,
outside managed activation locking. Existing global guides receive a managed
runtime note. Project-owned guides and local source changes are preserved.
Long-lived programs such as a separately installed audit web service are not
restarted or migrated by this updater; their deployment remains independent.
The small updater/launcher integration itself is upgraded by rerunning the
installer, while subsequent harness source versions activate automatically.

The developer checkout's existing `.harness-db` stays in place. The launcher
supplies explicit workflow, audit-outbox, dispatch and base-registry locations;
registered target databases retain their own scope. No database or secret files
are copied into releases. Machine-wide settings and credential locations stay
unchanged. Releases are retained for inspection and rollback; this version does
not garbage-collect older releases or interrupted staging directories.

## Status, rollback and development

```sh
harness-runtime status
harness-runtime check
harness-runtime rollback
harness-runtime override /absolute/developer/checkout
harness-runtime resume
```

Rollback selects the previous complete installation and pauses updates so the
next poll does not undo it. An explicit development override likewise pauses
updates and routes managed commands to that checkout. `resume` clears the pause;
the next check selects the latest eligible branch head. Rollback changes code,
not historical runtime state; it does not undo database migrations made by newer
code. A future incompatible migration needs an explicit migration strategy.

Status records the current/previous/override paths, last check time and the
reason for waiting or failure. Logs are in the systemd user journal or
`~/Library/Logs/harness-updater*.log` on macOS. User services run while the user
service manager is active; Linux operation after logout depends on the machine's
existing linger configuration.

## Local verification

```sh
cargo fmt --manifest-path services/harness-updater/Cargo.toml -- --check
cargo clippy --locked --manifest-path services/harness-updater/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path services/harness-updater/Cargo.toml
python3 services/harness-updater/tests/test_updater.py
python3 services/harness-updater/tests/test_hooks.py
python3 services/harness-updater/tests/test_installer.py
```

Real disposable Git repositories cover exact-commit CI gating, pending/failed
and wrong-commit evidence, workflow requirements, dirty-source preservation,
concurrent pollers, activation during a live operation, rollback, override,
launcher state paths and failed preparation. These checks are part of the local
pre-commit hook. No CI workflow is added or changed by the updater feature.
