use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;

use crate::config::{AppPaths, normalize_path, path_to_string};
use crate::db::{Database, NewRevisionFile, RevisionFile};
use crate::diff;
use crate::storage::{self, FileSnapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackOutcome {
    Tracked { path: String, revision_id: i64 },
    Reactivated { path: String },
    AlreadyTracked { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UntrackOutcome {
    Untracked { path: String },
    NotTracked { path: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum FileState {
    Clean,
    Modified,
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileStatusEntry {
    pub path: String,
    pub status: FileState,
    pub last_hash: Option<String>,
    pub current_hash: Option<String>,
    pub is_symlink: bool,
}

#[derive(Debug, Clone)]
pub struct FileDiffEntry {
    pub path: String,
    pub last_revision_id: i64,
    pub missing: bool,
    pub symlink_change: Option<String>,
    pub diff_text: String,
}

#[derive(Debug, Clone)]
pub struct SaveResult {
    pub revision_id: i64,
    pub saved_files: Vec<String>,
    pub skipped_unchanged: Vec<String>,
    pub skipped_missing: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub id: i64,
    pub timestamp: String,
    pub message: String,
    pub is_auto_backup: bool,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShowFileEntry {
    pub file_path: String,
    pub prev_hash: Option<String>,
    pub new_hash: String,
    pub file_mode: String,
    pub is_symlink: bool,
    pub symlink_target: Option<String>,
    pub diff: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShowResult {
    pub id: i64,
    pub timestamp: String,
    pub message: String,
    pub is_auto_backup: bool,
    pub files: Vec<ShowFileEntry>,
}

#[derive(Debug, Clone)]
pub struct RollbackTarget {
    pub file_path: String,
    pub target_hash: String,
    pub target_mode: u32,
    pub target_is_symlink: bool,
    pub target_symlink_target: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RollbackPlan {
    pub rev_id: i64,
    pub message: String,
    pub targets: Vec<RollbackTarget>,
    pub baseline_skipped: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RollbackResult {
    pub auto_backup_rev_id: Option<i64>,
    pub rollback_rev_id: Option<i64>,
    pub restored_files: Vec<String>,
    pub baseline_skipped: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RestorePlan {
    pub rev_id: i64,
    pub file_path: String,
    pub target_hash: String,
    pub target_mode: u32,
    pub target_is_symlink: bool,
    pub target_symlink_target: Option<String>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RestoreResult {
    pub auto_backup_rev_id: Option<i64>,
    pub restore_rev_id: i64,
    pub file_path: String,
}

pub fn init(paths: &AppPaths) -> Result<()> {
    fs::create_dir_all(&paths.config_dir).with_context(|| {
        format!(
            "failed to create config directory {}",
            paths.config_dir.display()
        )
    })?;
    fs::create_dir_all(paths.objects_dir()).with_context(|| {
        format!(
            "failed to create objects directory {}",
            paths.objects_dir().display()
        )
    })?;
    Database::init(&paths.db_path())?;
    Ok(())
}

pub fn track(paths: &AppPaths, raw_paths: &[PathBuf]) -> Result<Vec<TrackOutcome>> {
    let mut db = Database::open(&paths.db_path())?;
    let objects_dir = paths.objects_dir();
    let normalized = normalize_unique(raw_paths)?;
    let mut outcomes = Vec::with_capacity(normalized.len());

    for path in normalized {
        let path_str = path_to_string(&path)?;

        if let Some(existing) = db.get_tracked_file(&path_str)?
            && existing.active
        {
            outcomes.push(TrackOutcome::AlreadyTracked { path: path_str });
            continue;
        }

        let snap = storage::inspect_file(&path)?
            .ok_or_else(|| anyhow::anyhow!("file does not exist: {path_str}"))?;

        let now = now_rfc3339();
        if db.latest_revision_file(&path_str)?.is_some() {
            db.upsert_tracked_file(&path_str, &now)?;
            outcomes.push(TrackOutcome::Reactivated { path: path_str });
            continue;
        }

        storage::write_blob(&objects_dir, &snap.hash, &snap.bytes)?;

        let filename = display_filename(&path);
        let msg = format!("initial track: {filename}");
        let rev_file = NewRevisionFile {
            file_path: path_str.clone(),
            prev_hash: None,
            new_hash: snap.hash,
            file_mode: snap.mode,
            is_symlink: snap.is_symlink,
            symlink_target: snap.symlink_target,
        };
        let revision_id = db.create_revision(&now, &msg, false, &[&path_str], &[rev_file])?;
        outcomes.push(TrackOutcome::Tracked {
            path: path_str,
            revision_id,
        });
    }

    Ok(outcomes)
}

pub fn untrack(paths: &AppPaths, raw_paths: &[PathBuf]) -> Result<Vec<UntrackOutcome>> {
    let db = Database::open(&paths.db_path())?;
    let normalized = normalize_unique(raw_paths)?;
    let mut outcomes = Vec::with_capacity(normalized.len());

    for path in normalized {
        let path_str = path_to_string(&path)?;
        if db.set_tracked_inactive(&path_str)? {
            outcomes.push(UntrackOutcome::Untracked { path: path_str });
        } else {
            outcomes.push(UntrackOutcome::NotTracked { path: path_str });
        }
    }

    Ok(outcomes)
}

pub fn status(paths: &AppPaths) -> Result<Vec<FileStatusEntry>> {
    let db = Database::open(&paths.db_path())?;
    let tracked = db.list_active_tracked_files()?;
    let mut entries = Vec::with_capacity(tracked.len());

    for tf in tracked {
        let last = db.latest_revision_file(&tf.path)?;
        let disk = storage::inspect_file(Path::new(&tf.path))?;

        let entry = match (disk, last) {
            (None, last) => FileStatusEntry {
                path: tf.path,
                status: FileState::Missing,
                last_hash: last.as_ref().map(|r| r.new_hash.clone()),
                current_hash: None,
                is_symlink: last.as_ref().is_some_and(|r| r.is_symlink),
            },
            (Some(snap), Some(last)) => {
                let state = if is_changed(&snap, &last) {
                    FileState::Modified
                } else {
                    FileState::Clean
                };
                FileStatusEntry {
                    path: tf.path,
                    status: state,
                    last_hash: Some(last.new_hash),
                    current_hash: Some(snap.hash),
                    is_symlink: snap.is_symlink,
                }
            }
            (Some(snap), None) => FileStatusEntry {
                path: tf.path,
                status: FileState::Modified,
                last_hash: None,
                current_hash: Some(snap.hash),
                is_symlink: snap.is_symlink,
            },
        };
        entries.push(entry);
    }

    Ok(entries)
}

pub fn diff(paths: &AppPaths, raw_path: Option<&Path>, color: bool) -> Result<Vec<FileDiffEntry>> {
    let db = Database::open(&paths.db_path())?;
    let objects_dir = paths.objects_dir();

    let targets: Vec<String> = match raw_path {
        Some(raw) => {
            let norm = normalize_path(raw)?;
            let path_str = path_to_string(&norm)?;
            if db.latest_revision_file(&path_str)?.is_none() {
                bail!("no snapshot history found for '{path_str}'");
            }
            vec![path_str]
        }
        None => db
            .list_active_tracked_files()?
            .into_iter()
            .map(|tf| tf.path)
            .collect(),
    };

    let mut entries = Vec::new();
    for path_str in targets {
        let Some(last) = db.latest_revision_file(&path_str)? else {
            continue;
        };

        let Some(snap) = storage::inspect_file(Path::new(&path_str))? else {
            entries.push(FileDiffEntry {
                path: path_str,
                last_revision_id: last.revision_id,
                missing: true,
                symlink_change: None,
                diff_text: String::new(),
            });
            continue;
        };

        if !is_changed(&snap, &last) {
            continue;
        }

        let old_bytes = storage::read_blob(&objects_dir, &last.new_hash)?;
        let old_label = format!("a/{path_str} (rev #{})", last.revision_id);
        let new_label = format!("b/{path_str} (working)");
        let diff_text = diff::unified_diff(&old_bytes, &snap.bytes, &old_label, &new_label, color);
        let symlink_change = format_symlink_change(
            last.is_symlink,
            last.symlink_target.as_deref(),
            snap.is_symlink,
            snap.symlink_target.as_deref(),
        );

        entries.push(FileDiffEntry {
            path: path_str,
            last_revision_id: last.revision_id,
            missing: false,
            symlink_change,
            diff_text,
        });
    }

    Ok(entries)
}

pub fn save(paths: &AppPaths, raw_paths: &[PathBuf], message: &str) -> Result<Option<SaveResult>> {
    let msg = message.trim();
    if msg.is_empty() {
        bail!("commit message cannot be empty");
    }

    let mut db = Database::open(&paths.db_path())?;
    let objects_dir = paths.objects_dir();
    let now = now_rfc3339();

    let mut pending_blobs: Vec<(String, Vec<u8>)> = Vec::new();
    let mut new_rev_files: Vec<NewRevisionFile> = Vec::new();
    let mut paths_to_track: Vec<String> = Vec::new();
    let mut skipped_unchanged: Vec<String> = Vec::new();
    let mut skipped_missing: Vec<String> = Vec::new();

    if !raw_paths.is_empty() {
        let normalized = normalize_unique(raw_paths)?;
        for path in normalized {
            let path_str = path_to_string(&path)?;
            let snap = storage::inspect_file(&path)?
                .ok_or_else(|| anyhow::anyhow!("file does not exist: {path_str}"))?;

            let last = db.latest_revision_file(&path_str)?;
            if let Some(ref prev) = last
                && !is_changed(&snap, prev)
            {
                // Still ensure explicitly named path is active in watchlist.
                db.upsert_tracked_file(&path_str, &now)?;
                skipped_unchanged.push(path_str);
                continue;
            }

            paths_to_track.push(path_str.clone());
            pending_blobs.push((snap.hash.clone(), snap.bytes));
            new_rev_files.push(NewRevisionFile {
                file_path: path_str,
                prev_hash: last.map(|r| r.new_hash),
                new_hash: snap.hash,
                file_mode: snap.mode,
                is_symlink: snap.is_symlink,
                symlink_target: snap.symlink_target,
            });
        }
    } else {
        for tf in db.list_active_tracked_files()? {
            let Some(snap) = storage::inspect_file(Path::new(&tf.path))? else {
                skipped_missing.push(tf.path);
                continue;
            };
            let last = db.latest_revision_file(&tf.path)?;
            if let Some(ref prev) = last
                && !is_changed(&snap, prev)
            {
                skipped_unchanged.push(tf.path);
                continue;
            }

            pending_blobs.push((snap.hash.clone(), snap.bytes));
            new_rev_files.push(NewRevisionFile {
                file_path: tf.path,
                prev_hash: last.map(|r| r.new_hash),
                new_hash: snap.hash,
                file_mode: snap.mode,
                is_symlink: snap.is_symlink,
                symlink_target: snap.symlink_target,
            });
        }
    }

    if new_rev_files.is_empty() {
        return Ok(None);
    }

    for (hash, bytes) in &pending_blobs {
        storage::write_blob(&objects_dir, hash, bytes)?;
    }

    let track_refs: Vec<&str> = paths_to_track.iter().map(String::as_str).collect();
    let revision_id = db.create_revision(&now, msg, false, &track_refs, &new_rev_files)?;
    let saved_files = new_rev_files.into_iter().map(|f| f.file_path).collect();

    Ok(Some(SaveResult {
        revision_id,
        saved_files,
        skipped_unchanged,
        skipped_missing,
    }))
}

pub fn log(paths: &AppPaths, raw_path: Option<&Path>, limit: Option<usize>) -> Result<Vec<LogEntry>> {
    let db = Database::open(&paths.db_path())?;
    let filter = match raw_path {
        Some(raw) => Some(path_to_string(&normalize_path(raw)?)?),
        None => None,
    };

    let rows = db.list_revisions(filter.as_deref(), limit)?;
    Ok(rows
        .into_iter()
        .map(|(rev, files)| LogEntry {
            id: rev.id,
            timestamp: rev.timestamp,
            message: rev.message,
            is_auto_backup: rev.is_auto_backup,
            files: files.into_iter().map(|f| f.file_path).collect(),
        })
        .collect())
}

pub fn show(paths: &AppPaths, rev_id: i64, color: bool) -> Result<ShowResult> {
    let db = Database::open(&paths.db_path())?;
    let objects_dir = paths.objects_dir();

    let rev = db
        .get_revision(rev_id)?
        .ok_or_else(|| anyhow::anyhow!("revision #{rev_id} not found"))?;
    let rev_files = db.get_revision_files(rev_id)?;

    let mut entries = Vec::with_capacity(rev_files.len());
    for rf in rev_files {
        let old_bytes = match &rf.prev_hash {
            Some(prev) => storage::read_blob(&objects_dir, prev)?,
            None => Vec::new(),
        };
        let new_bytes = storage::read_blob(&objects_dir, &rf.new_hash)?;

        let old_label = match &rf.prev_hash {
            Some(_) => format!("a/{} (before rev #{rev_id})", rf.file_path),
            None => format!("/dev/null (initial baseline for {})", rf.file_path),
        };
        let new_label = format!("b/{} (rev #{rev_id})", rf.file_path);
        let diff_text = diff::unified_diff(&old_bytes, &new_bytes, &old_label, &new_label, color);

        entries.push(ShowFileEntry {
            file_path: rf.file_path,
            prev_hash: rf.prev_hash,
            new_hash: rf.new_hash,
            file_mode: format!("{:04o}", rf.file_mode),
            is_symlink: rf.is_symlink,
            symlink_target: rf.symlink_target,
            diff: diff_text,
        });
    }

    Ok(ShowResult {
        id: rev.id,
        timestamp: rev.timestamp,
        message: rev.message,
        is_auto_backup: rev.is_auto_backup,
        files: entries,
    })
}

pub fn plan_rollback(paths: &AppPaths, rev_id: i64) -> Result<RollbackPlan> {
    let db = Database::open(&paths.db_path())?;
    let rev = db
        .get_revision(rev_id)?
        .ok_or_else(|| anyhow::anyhow!("revision #{rev_id} not found"))?;
    let rev_files = db.get_revision_files(rev_id)?;

    let mut targets = Vec::new();
    let mut baseline_skipped = Vec::new();
    let mut warnings = Vec::new();

    for rf in rev_files {
        let Some(prev_hash) = rf.prev_hash else {
            baseline_skipped.push(rf.file_path);
            continue;
        };

        let prior = db.previous_file_state(&rf.file_path, rev_id, &prev_hash)?;
        let (target_mode, target_is_symlink, target_symlink_target) = match prior {
            Some(p) => (p.file_mode, p.is_symlink, p.symlink_target),
            None => (rf.file_mode, rf.is_symlink, rf.symlink_target.clone()),
        };

        match storage::inspect_file(Path::new(&rf.file_path))? {
            None => warnings.push(format!(
                "'{}' is currently missing on disk (expected state from rev #{rev_id})",
                rf.file_path
            )),
            Some(disk)
                if disk.hash != rf.new_hash
                    || disk.is_symlink != rf.is_symlink
                    || disk.symlink_target != rf.symlink_target =>
            {
                warnings.push(format!(
                    "'{}' has been modified on disk since rev #{rev_id} (disk hash differs from rev #{rev_id})",
                    rf.file_path
                ));
            }
            Some(_) => {}
        }

        targets.push(RollbackTarget {
            file_path: rf.file_path,
            target_hash: prev_hash,
            target_mode,
            target_is_symlink,
            target_symlink_target,
        });
    }

    Ok(RollbackPlan {
        rev_id: rev.id,
        message: rev.message,
        targets,
        baseline_skipped,
        warnings,
    })
}

pub fn execute_rollback(paths: &AppPaths, plan: &RollbackPlan) -> Result<RollbackResult> {
    if plan.targets.is_empty() {
        return Ok(RollbackResult {
            auto_backup_rev_id: None,
            rollback_rev_id: None,
            restored_files: Vec::new(),
            baseline_skipped: plan.baseline_skipped.clone(),
            warnings: plan.warnings.clone(),
        });
    }

    let mut db = Database::open(&paths.db_path())?;
    let objects_dir = paths.objects_dir();
    let now = now_rfc3339();

    // Phase 1: Safety First — persist auto-backup snapshot of current disk state BEFORE writing disk.
    let mut backup_files = Vec::new();
    let mut pre_disk_hashes: Vec<Option<String>> = Vec::with_capacity(plan.targets.len());

    for target in &plan.targets {
        if let Some(disk) = storage::inspect_file(Path::new(&target.file_path))? {
            storage::write_blob(&objects_dir, &disk.hash, &disk.bytes)?;
            let last = db.latest_revision_file(&target.file_path)?;
            backup_files.push(NewRevisionFile {
                file_path: target.file_path.clone(),
                prev_hash: last.map(|r| r.new_hash),
                new_hash: disk.hash.clone(),
                file_mode: disk.mode,
                is_symlink: disk.is_symlink,
                symlink_target: disk.symlink_target,
            });
            pre_disk_hashes.push(Some(disk.hash));
        } else {
            pre_disk_hashes.push(None);
        }
    }

    let auto_backup_rev_id = if !backup_files.is_empty() {
        let msg = format!("auto-backup before rollback #{}", plan.rev_id);
        Some(db.create_revision(&now, &msg, true, &[], &backup_files)?)
    } else {
        None
    };

    // Phase 2: Atomically restore each target file to its prior state.
    for target in &plan.targets {
        storage::restore_file_atomically(
            &objects_dir,
            Path::new(&target.file_path),
            &target.target_hash,
            target.target_mode,
            target.target_is_symlink,
            target.target_symlink_target.as_deref(),
        )?;
    }

    // Phase 3: Record the rolled-back state so `status` is CLEAN and rollback itself is reversible.
    let applied_files: Vec<NewRevisionFile> = plan
        .targets
        .iter()
        .zip(pre_disk_hashes)
        .map(|(target, pre_hash)| NewRevisionFile {
            file_path: target.file_path.clone(),
            prev_hash: pre_hash,
            new_hash: target.target_hash.clone(),
            file_mode: target.target_mode,
            is_symlink: target.target_is_symlink,
            symlink_target: target.target_symlink_target.clone(),
        })
        .collect();

    let rollback_msg = format!("rollback #{}", plan.rev_id);
    let rollback_rev_id = Some(db.create_revision(
        &now_rfc3339(),
        &rollback_msg,
        false,
        &[],
        &applied_files,
    )?);

    let restored_files = plan.targets.iter().map(|t| t.file_path.clone()).collect();

    Ok(RollbackResult {
        auto_backup_rev_id,
        rollback_rev_id,
        restored_files,
        baseline_skipped: plan.baseline_skipped.clone(),
        warnings: plan.warnings.clone(),
    })
}

pub fn rollback(paths: &AppPaths, rev_id: i64) -> Result<RollbackResult> {
    let plan = plan_rollback(paths, rev_id)?;
    execute_rollback(paths, &plan)
}

pub fn plan_restore(paths: &AppPaths, raw_path: &Path, rev_id: i64) -> Result<RestorePlan> {
    let db = Database::open(&paths.db_path())?;
    let norm = normalize_path(raw_path)?;
    let path_str = path_to_string(&norm)?;

    if db.get_revision(rev_id)?.is_none() {
        bail!("revision #{rev_id} not found");
    }

    let rf = db.get_revision_file(rev_id, &path_str)?.ok_or_else(|| {
        anyhow::anyhow!("file '{path_str}' was not recorded in revision #{rev_id}")
    })?;

    let warning = match (
        storage::inspect_file(Path::new(&path_str))?,
        db.latest_revision_file(&path_str)?,
    ) {
        (Some(disk), Some(last)) if is_changed(&disk, &last) => Some(format!(
            "'{path_str}' has uncommitted modifications on disk (will be auto-backed up first)"
        )),
        _ => None,
    };

    Ok(RestorePlan {
        rev_id,
        file_path: path_str,
        target_hash: rf.new_hash,
        target_mode: rf.file_mode,
        target_is_symlink: rf.is_symlink,
        target_symlink_target: rf.symlink_target,
        warning,
    })
}

pub fn execute_restore(paths: &AppPaths, plan: &RestorePlan) -> Result<RestoreResult> {
    let mut db = Database::open(&paths.db_path())?;
    let objects_dir = paths.objects_dir();
    let now = now_rfc3339();

    // Phase 1: Auto-backup current disk state before overwriting.
    let (auto_backup_rev_id, pre_disk_hash) =
        if let Some(disk) = storage::inspect_file(Path::new(&plan.file_path))? {
            storage::write_blob(&objects_dir, &disk.hash, &disk.bytes)?;
            let last = db.latest_revision_file(&plan.file_path)?;
            let backup_file = NewRevisionFile {
                file_path: plan.file_path.clone(),
                prev_hash: last.map(|r| r.new_hash),
                new_hash: disk.hash.clone(),
                file_mode: disk.mode,
                is_symlink: disk.is_symlink,
                symlink_target: disk.symlink_target,
            };
            let msg = format!("auto-backup before restore #{}", plan.rev_id);
            let id = db.create_revision(&now, &msg, true, &[], &[backup_file])?;
            (Some(id), Some(disk.hash))
        } else {
            (None, None)
        };

    // Phase 2: Atomic restore to disk.
    storage::restore_file_atomically(
        &objects_dir,
        Path::new(&plan.file_path),
        &plan.target_hash,
        plan.target_mode,
        plan.target_is_symlink,
        plan.target_symlink_target.as_deref(),
    )?;

    // Phase 3: Record restored state.
    let filename = display_filename(Path::new(&plan.file_path));
    let applied = NewRevisionFile {
        file_path: plan.file_path.clone(),
        prev_hash: pre_disk_hash,
        new_hash: plan.target_hash.clone(),
        file_mode: plan.target_mode,
        is_symlink: plan.target_is_symlink,
        symlink_target: plan.target_symlink_target.clone(),
    };
    let msg = format!("restore {filename} to #{}", plan.rev_id);
    let restore_rev_id = db.create_revision(&now_rfc3339(), &msg, false, &[], &[applied])?;

    Ok(RestoreResult {
        auto_backup_rev_id,
        restore_rev_id,
        file_path: plan.file_path.clone(),
    })
}

pub fn restore(paths: &AppPaths, raw_path: &Path, rev_id: i64) -> Result<RestoreResult> {
    let plan = plan_restore(paths, raw_path, rev_id)?;
    execute_restore(paths, &plan)
}

fn is_changed(snap: &FileSnapshot, last: &RevisionFile) -> bool {
    snap.hash != last.new_hash
        || snap.is_symlink != last.is_symlink
        || snap.symlink_target != last.symlink_target
}

fn normalize_unique(raw_paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(raw_paths.len());
    for raw in raw_paths {
        let norm = normalize_path(raw)?;
        if seen.insert(norm.clone()) {
            out.push(norm);
        }
    }
    Ok(out)
}

fn display_filename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn format_symlink_change(
    old_is_symlink: bool,
    old_target: Option<&str>,
    new_is_symlink: bool,
    new_target: Option<&str>,
) -> Option<String> {
    if old_is_symlink == new_is_symlink && old_target == new_target {
        return None;
    }
    match (old_is_symlink, new_is_symlink) {
        (true, true) => Some(format!(
            "symlink target changed: {} -> {}",
            old_target.unwrap_or("<none>"),
            new_target.unwrap_or("<none>")
        )),
        (false, true) => Some(format!(
            "regular file replaced with symlink -> {}",
            new_target.unwrap_or("<none>")
        )),
        (true, false) => Some(format!(
            "symlink (-> {}) replaced with regular file",
            old_target.unwrap_or("<none>")
        )),
        (false, false) => None,
    }
}
