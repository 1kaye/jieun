# `jieun` — File-Scoped Config VCS

`jieun` is a fast, lightweight, file-scoped Version Control System written in Rust for Linux configuration files (Omarchy, Hyprland, Waybar, `herdr`, `agy`, dotfiles, and local scripts).

Unlike Git's whole-tree snapshot model, `jieun` tracks only explicitly registered files and isolates every snapshot and rollback to the exact files involved.

## Core Principles

- **Whitelist / Opt-in Tracking:** Never scans `~` or `~/.config`. Only files explicitly registered via `track` or `save <path>` are monitored; system updates to neighboring files never pollute `status` or `diff`.
- **Isolated Snapshots:** `save` records only files whose content (BLAKE3 hash) or symlink target actually changed since their previous snapshot.
- **Selective Rollback:** Rolling back revision `#N` restores **only** the files changed in `#N`, leaving changes made to other files before or after completely untouched.
- **Safety First:** Every `rollback` and `restore` creates an `auto-backup` revision of the current disk state **before** overwriting anything, so rollbacks themselves can always be undone.

## Storage Architecture (XDG Base Directory)

Application data is stored outside `~/.config/` to avoid conflicts and watcher loops:

```text
~/.config/jieun/
└── config.toml            # Optional lightweight config

~/.local/share/jieun/
├── history.db             # SQLite (WAL + foreign keys) for watchlist & revision logs
└── objects/               # Content-Addressable Storage (BLAKE3 + Zstd)
    └── <2-char-prefix>/
        └── <blake3-hash>.zst
```

Environment overrides for custom setups or isolated testing:
- `JIEUN_CONFIG_DIR` — overrides `~/.config/jieun`
- `JIEUN_DATA_DIR` — overrides `~/.local/share/jieun`

## Installation

```bash
cargo build --release
# Binary will be at target/release/jieun (or configured cargo target-dir)
```

Or install directly into `~/.cargo/bin`:

```bash
cargo install --path .
```

## CLI Usage

Global flag: `--json` is supported by `status`, `log`, and `show` for machine-readable output.

### 1. Initialize

```bash
jieun init
```

Idempotent. Creates XDG directories and initializes `history.db`.

### 2. Track & Untrack Files

```bash
# Register one or more files and capture their initial baseline snapshot
jieun track ~/.config/hypr/hyprland.conf ~/.local/bin/launch.sh

# Stop active monitoring without deleting revision history
jieun untrack ~/.config/hypr/hyprland.conf
jieun untrack ~/.config/hypr/hyprland.conf --yes
```

### 3. Inspect Status & Diff

```bash
# Check CLEAN / MODIFIED / MISSING state of actively tracked files
jieun status
jieun status --json

# Colored unified diff against latest snapshot (all modified files or a single path)
jieun diff
jieun diff ~/.config/hypr/hyprland.conf
```

### 4. Save Isolated Snapshots

```bash
# Auto-track (if needed) and snapshot specific file(s)
jieun save ~/.config/herdr/config.toml -m "mengubah herdr"

# Snapshot all currently MODIFIED tracked files
jieun save -m "update config agy"
```

If no files have changed, no empty revision is created.

### 5. Browse History & Inspect Revisions

```bash
# List revisions (optionally filtered to a file and/or limited)
jieun log
jieun log ~/.config/herdr/config.toml -n 10
jieun log --json

# Show full metadata and unified diff for a specific revision
jieun show 2
jieun show 2 --json
```

### 6. Selective Rollback & Single-File Restore

```bash
# Roll back only the files modified in revision #2 to their prior state
jieun rollback 2
jieun rollback 2 --yes

# Restore a single file to its recorded state at revision #1
jieun restore ~/.config/herdr/config.toml --to 1
jieun restore ~/.config/herdr/config.toml --to 1 --yes
```

## Linux Safety Guarantees

- **Atomic Writes:** Writes restored content to a sibling temporary file (`.jieun.tmp.<random>`), applies recorded Unix permissions, calls `sync_all()`, and atomically replaces the target via `rename()` — preventing live-reload daemons (Hyprland, Waybar, etc.) from reading partial files.
- **Permission Preservation:** Records and restores Unix permission bits (`0644`, `0755`, etc.) via `PermissionsExt`.
- **Symlink Awareness:** Uses `symlink_metadata()` and lexical path normalization (never dereferencing symlinks via `canonicalize`) so symlink targets and structures stay intact across rollbacks and restores.
- **Baseline Protection:** Rolling back an initial tracking revision (`prev_hash = NULL`) never deletes the file on disk.
- **Drift Detection:** Warns before rolling back if a file on disk was modified after the target revision.

## Development & Testing

```bash
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```
