# AI Agent Instructions: `jieun` (File-Scoped Linux Config VCS)

`jieun` is a lightweight, file-scoped Version Control System for **personal Linux configuration files, dotfiles, and standalone user scripts** (e.g., Hyprland, Omarchy, Waybar, `herdr`, `agy`, `~/.zshrc`, `~/.local/bin/*`).

---

## 1. Strict Scope: When to Use `jieun` vs `kaye` / `git`

**NEVER confuse `jieun` with project version control.**

| Target | Tool to Use | Never Use |
| --- | --- | --- |
| Linux configs, dotfiles, user scripts (`~/.config/*`, `~/.zshrc`, `~/.local/bin/*`) | **`jieun`** | `kaye`, `git` (unless already a dotfiles repo) |
| Software projects, source code repos, workspace experiments | **`kaye`** (checkpoints) / **`git`** (final commits) | **`jieun`** |

### Non-Negotiable Scope Rules

1. **NEVER use `jieun` inside software projects or Git repositories:** Do not run `jieun track` or `jieun save` on project source files (`src/`, `Cargo.toml`, `package.json`, etc.). Use `kaye` for project experiments instead.
2. **NEVER glob or bulk-track directories:** `jieun` is strictly **whitelist / file-scoped**. Only register the exact config file(s) you are actually going to modify (never run `jieun track ~/.config/*` or pass a directory — directories are rejected by design).
3. **NEVER use `jieun` without an explicit file reason:** If you are only reading a config file, do **not** run `jieun track` or `jieun save` on it.

---

## 2. Architecture Context for AI Agents

- **Isolated per-file snapshots:** Unlike Git's whole-tree snapshots, every `jieun` revision records only the specific file(s) whose BLAKE3 hash or symlink target actually changed.
- **Selective rollback:** `jieun rollback <rev_id>` reverts **only** the file(s) touched in `<rev_id>`, without touching any other config file changed before or after.
- **Storage (XDG Base Directory):**
  - Metadata & watchlist: `~/.local/share/jieun/history.db` (SQLite WAL)
  - Compressed content blobs: `~/.local/share/jieun/objects/<2-char>/<blake3>.zst` (Zstd CAS)
  - Optional tool config: `~/.config/jieun/config.toml`
- **Safety guarantees:**
  - **Atomic writes** (`.jieun.tmp.<random>` → apply Unix mode → `sync_all()` → `rename()`) prevent live-reload apps (Hyprland, Waybar, etc.) from reading corrupted/partial configs.
  - **Auto-backup before overwrite:** Both `rollback` and `restore` commit an `auto-backup` revision (`is_auto_backup = 1`) of the current disk state **before** modifying disk.
  - **Baseline protection:** Rolling back an initial `track` revision (`prev_hash = NULL`) never deletes the file on disk.
  - **Symlink & permission preservation:** Symlink structure and Unix modes (e.g. `0755` scripts) are preserved automatically.

---

## 3. Strict Operational Guardrails

1. **Capture a baseline BEFORE editing a config file:**
   - Before modifying any Linux config file, ensure its current working state is recorded (`jieun track <path>` if untracked).
   - If the file is already tracked and currently `MODIFIED` (`jieun status --json`), inspect `jieun diff <path>` first. Do **not** blindly lump pre-existing user edits with your own edits — snapshot the user's pre-existing state first or ask the user before proceeding.
2. **Always target explicit file path(s) on `save`:**
   - Prefer `jieun save <path> -m "<message>"` over bare `jieun save -m "<message>"`.
   - Bare `jieun save -m "..."` snapshots **all** currently `MODIFIED` tracked files — only use it after verifying `jieun status --json` shows exclusively the file(s) you intend to snapshot.
   - Always pass a clear, concise `-m "<message>"` (empty messages fail).
3. **Prefer `--json` when inspecting state programmatically:**
   - Use `jieun status --json`, `jieun log [path] [-n N] --json`, and `jieun show <rev_id> --json` for reliable parsing.
4. **Do NOT run `rollback`, `restore`, or `untrack` recklessly:**
   - Always inspect `jieun show <rev_id> --json` (or `jieun log <path> --json`) before reverting a config.
   - `rollback`, `restore`, and `untrack` require interactive `[y/N]` confirmation unless `--yes` (`-y`) is passed.
   - Only run `jieun rollback <rev_id> --yes` or `jieun restore <path> --to <rev_id> --yes` when explicitly instructed by the user OR to recover a config you just broke during the current task.
   - **Never** run `jieun untrack <path> --yes` unless explicitly asked by the user.
5. **Never edit `~/.local/share/jieun/` directly:** Always go through the `jieun` CLI so SQLite records and CAS blobs stay consistent.

---

## 4. Standard Agent Workflow (When Editing Linux Configs)

### Step 1: Verify `jieun` Is Ready

```bash
jieun status --json
```

If `jieun` reports it is not initialized yet, initialize it once (idempotent):

```bash
jieun init
```

### Step 2: Capture Baseline Before Editing

Check whether the target config file is already tracked and clean:

```bash
jieun status --json
jieun log ~/.config/hypr/hyprland.conf -n 3 --json
```

- **If NOT tracked yet:** track it **before** editing so the original working config becomes the rollbackable baseline (`"initial track: <filename>"`):
  ```bash
  jieun track ~/.config/hypr/hyprland.conf
  ```
- **If ALREADY tracked and `MODIFIED`:** check `jieun diff ~/.config/hypr/hyprland.conf`. If there are unrelated user edits, save them first as their own revision before applying your changes:
  ```bash
  jieun save ~/.config/hypr/hyprland.conf -m "backup uncommitted user edits before <task>"
  ```

### Step 3: Edit, Inspect Diff, and Save Isolated Snapshot

After editing the config file:

```bash
# Review exact unified diff against latest snapshot
jieun diff ~/.config/hypr/hyprland.conf

# Save ONLY the file(s) you edited (skips automatically if unchanged)
jieun save ~/.config/hypr/hyprland.conf -m "adjust hyprland gaps_in to 8"
```

> Note: If you edit a file that is not tracked yet and forgot `jieun track`, `jieun save <path> -m "..."` auto-tracks it — **but** the pre-edit state will be lost because no baseline existed before your edit. Always `track` **before** editing!

### Step 4: Inspect History & Revision Details

```bash
# List recent revisions globally or for a single file
jieun log -n 10 --json
jieun log ~/.config/herdr/config.toml -n 5 --json

# Inspect metadata, file mode, symlink target, and unified diff of a revision
jieun show <rev_id> --json
```

### Step 5: Selective Rollback or Single-File Restore

Before reverting, inspect the target revision:

```bash
jieun show <rev_id>
```

- **Undo a specific revision (affects ONLY files recorded in `<rev_id>`):**
  ```bash
  jieun rollback <rev_id> --yes
  ```
  - Creates an `[auto-backup]` revision (`"auto-backup before rollback #<rev_id>"`) **before** writing disk, then records `"rollback #<rev_id>"`.
  - Warns if the file on disk was modified again after `<rev_id>`.
  - Never deletes a file whose `prev_hash` is `NULL` (baseline).

- **Restore one specific file to its exact state in `<rev_id>`:**
  ```bash
  jieun restore ~/.config/herdr/config.toml --to <rev_id> --yes
  ```
  - Also creates an `[auto-backup]` revision first, then restores the file atomically with its original permissions/symlink target.

- **Undo a rollback itself (if the rollback made things worse):**
  ```bash
  jieun log ~/.config/herdr/config.toml -n 5 --json
  # Either roll back the "rollback #<N>" revision:
  jieun rollback <rollback_rev_id> --yes
  # Or restore directly from the "[auto-backup]" revision:
  jieun restore ~/.config/herdr/config.toml --to <auto_backup_rev_id> --yes
  ```

### Step 6: Stop Tracking a File (Only If User Requests)

```bash
jieun untrack ~/.config/herdr/config.toml --yes
```

Stops monitoring in `status` and bare `save`, while preserving all historical revisions in SQLite so `log`, `show`, and `restore` still work.

---

## 5. Quick Command Reference

| Command | Purpose |
| --- | --- |
| `jieun init` | Initialize XDG dirs & SQLite DB (idempotent) |
| `jieun track <path>...` | Register file(s) & save initial baseline snapshot |
| `jieun untrack <path>... [--yes]` | Stop active tracking (keeps revision history) |
| `jieun status [--json]` | Show `CLEAN` / `MODIFIED` / `MISSING` for tracked files |
| `jieun diff [path]` | Unified diff of working file(s) vs latest snapshot |
| `jieun save [path...] -m "<msg>"` | Snapshot changed file(s); no-op if unchanged |
| `jieun log [path] [-n N] [--json]` | List revisions (newest first), optionally per file |
| `jieun show <rev_id> [--json]` | Show revision metadata & unified diff |
| `jieun rollback <rev_id> [--yes]` | Revert only `<rev_id>` files to `prev_hash` (auto-backup first) |
| `jieun restore <path> --to <rev_id> [--yes]` | Restore `<path>` to `<rev_id>`'s `new_hash` (auto-backup first) |
