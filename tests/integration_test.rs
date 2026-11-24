use std::fs::{self, Permissions};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::Command;

use jieun::config::AppPaths;
use jieun::ops::{self, FileState};
use tempfile::TempDir;

struct TestEnv {
    _root: TempDir,
    work_dir: PathBuf,
    paths: AppPaths,
}

impl TestEnv {
    fn new() -> Self {
        let root = TempDir::new().expect("create temp dir");
        let work_dir = root.path().join("configs");
        let config_dir = root.path().join("xdg_config/jieun");
        let data_dir = root.path().join("xdg_data/jieun");
        fs::create_dir_all(&work_dir).expect("create work dir");

        let paths = AppPaths::from_dirs(config_dir, data_dir);
        ops::init(&paths).expect("init jieun");

        Self {
            _root: root,
            work_dir,
            paths,
        }
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.work_dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&p, content).expect("write file");
        p
    }

    fn read(&self, path: &PathBuf) -> String {
        fs::read_to_string(path).expect("read file")
    }
}

#[test]
fn untracked_sibling_files_are_never_detected_by_status_or_save() {
    let env = TestEnv::new();
    let tracked = env.write("herdr/config.toml", "mode = 'fast'\n");
    let untracked = env.write("herdr/system-generated.toml", "auto = true\n");

    ops::track(&env.paths, std::slice::from_ref(&tracked)).unwrap();

    // Mutate untracked sibling file and add another untracked file in the same directory.
    fs::write(&untracked, "auto = false\n").unwrap();
    env.write("herdr/another-untracked.conf", "noise = 1\n");

    let statuses = ops::status(&env.paths).unwrap();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].path, tracked.to_str().unwrap());
    assert_eq!(statuses[0].status, FileState::Clean);

    // `save` with no paths must not snapshot untracked files and must not create a revision.
    let saved = ops::save(&env.paths, &[], "should not capture untracked").unwrap();
    assert!(saved.is_none());

    // Modify tracked file and verify only tracked file enters the new revision.
    fs::write(&tracked, "mode = 'turbo'\n").unwrap();
    let saved = ops::save(&env.paths, &[], "update herdr")
        .unwrap()
        .expect("revision created");
    assert_eq!(saved.saved_files, vec![tracked.to_str().unwrap().to_string()]);
}

#[test]
fn selective_rollback_of_file_a_does_not_touch_file_b() {
    let env = TestEnv::new();
    let file_a = env.write("hypr/hyprland.conf", "gaps_in = 5\n");
    let file_b = env.write("waybar/config.jsonc", "{ \"height\": 28 }\n");

    ops::track(&env.paths, &[file_a.clone(), file_b.clone()]).unwrap();

    // Revision for File A
    fs::write(&file_a, "gaps_in = 12\n").unwrap();
    let rev_a = ops::save(&env.paths, std::slice::from_ref(&file_a), "change hypr gaps")
        .unwrap()
        .unwrap()
        .revision_id;

    // Separate revision for File B
    fs::write(&file_b, "{ \"height\": 36 }\n").unwrap();
    let _rev_b = ops::save(&env.paths, std::slice::from_ref(&file_b), "increase waybar height")
        .unwrap()
        .unwrap()
        .revision_id;

    // Roll back ONLY File A's revision
    let res = ops::rollback(&env.paths, rev_a).unwrap();
    assert_eq!(res.restored_files, vec![file_a.to_str().unwrap().to_string()]);

    // File A reverted; File B completely untouched
    assert_eq!(env.read(&file_a), "gaps_in = 5\n");
    assert_eq!(env.read(&file_b), "{ \"height\": 36 }\n");

    let statuses = ops::status(&env.paths).unwrap();
    assert!(statuses.iter().all(|s| s.status == FileState::Clean));
}

#[test]
fn rollback_creates_valid_auto_backup_that_can_be_rolled_back_again() {
    let env = TestEnv::new();
    let file_a = env.write("agy/settings.toml", "theme = 'dark'\n");

    ops::track(&env.paths, std::slice::from_ref(&file_a)).unwrap();

    fs::write(&file_a, "theme = 'solarized'\n").unwrap();
    let rev_edit = ops::save(&env.paths, &[], "switch to solarized")
        .unwrap()
        .unwrap()
        .revision_id;

    // Add uncommitted edit on disk before rolling back to verify auto-backup protects it
    fs::write(&file_a, "theme = 'nord-draft'\n").unwrap();

    let plan = ops::plan_rollback(&env.paths, rev_edit).unwrap();
    assert!(
        !plan.warnings.is_empty(),
        "expected drift warning when disk differs from rev #{rev_edit}"
    );

    let rb1 = ops::execute_rollback(&env.paths, &plan).unwrap();
    assert_eq!(env.read(&file_a), "theme = 'dark'\n");

    let backup_id = rb1.auto_backup_rev_id.expect("auto-backup rev must exist");
    let rollback_rev_id = rb1.rollback_rev_id.expect("rollback rev must exist");

    let logs = ops::log(&env.paths, None, None).unwrap();
    let backup_entry = logs
        .iter()
        .find(|e| e.id == backup_id)
        .expect("auto-backup must appear in log");
    assert!(backup_entry.is_auto_backup);
    assert_eq!(
        backup_entry.message,
        format!("auto-backup before rollback #{rev_edit}")
    );

    // Undo the rollback itself by rolling back the rollback revision -> recovers 'nord-draft'
    let rb2 = ops::rollback(&env.paths, rollback_rev_id).unwrap();
    assert!(rb2.auto_backup_rev_id.is_some());
    assert_eq!(env.read(&file_a), "theme = 'nord-draft'\n");

    // Also verify `restore --to <auto_backup_id>` directly recovers the backed-up content
    fs::write(&file_a, "theme = 'temporary'\n").unwrap();
    ops::restore(&env.paths, &file_a, backup_id).unwrap();
    assert_eq!(env.read(&file_a), "theme = 'nord-draft'\n");
}

#[test]
fn rollback_preserves_executable_permissions_0755() {
    let env = TestEnv::new();
    let script = env.write("bin/launch.sh", "#!/bin/sh\necho v1\n");
    fs::set_permissions(&script, Permissions::from_mode(0o755)).unwrap();

    ops::track(&env.paths, std::slice::from_ref(&script)).unwrap();

    // Update script content and downgrade permissions to 0644
    fs::write(&script, "#!/bin/sh\necho v2\n").unwrap();
    fs::set_permissions(&script, Permissions::from_mode(0o644)).unwrap();
    let rev_v2 = ops::save(&env.paths, &[], "update launch script")
        .unwrap()
        .unwrap()
        .revision_id;

    ops::rollback(&env.paths, rev_v2).unwrap();

    assert_eq!(env.read(&script), "#!/bin/sh\necho v1\n");
    let restored_mode = fs::metadata(&script).unwrap().permissions().mode() & 0o7777;
    assert_eq!(restored_mode, 0o755);
}

#[test]
fn saving_unchanged_file_does_not_create_empty_revision() {
    let env = TestEnv::new();
    let cfg = env.write("herdr/config.toml", "workers = 4\n");

    ops::track(&env.paths, std::slice::from_ref(&cfg)).unwrap();
    let before_count = ops::log(&env.paths, None, None).unwrap().len();
    assert_eq!(before_count, 1);

    // Save without path args
    assert!(ops::save(&env.paths, &[], "no-op save all").unwrap().is_none());
    // Save with explicit unchanged path
    assert!(
        ops::save(&env.paths, std::slice::from_ref(&cfg), "no-op save explicit")
            .unwrap()
            .is_none()
    );

    let after_count = ops::log(&env.paths, None, None).unwrap().len();
    assert_eq!(after_count, before_count);
}

#[test]
fn rollback_baseline_does_not_delete_file() {
    let env = TestEnv::new();
    let cfg = env.write("dotfiles/.zshrc", "export EDITOR=nvim\n");

    let outcomes = ops::track(&env.paths, std::slice::from_ref(&cfg)).unwrap();
    let baseline_id = match &outcomes[0] {
        ops::TrackOutcome::Tracked { revision_id, .. } => *revision_id,
        other => panic!("unexpected track outcome: {other:?}"),
    };

    let rb = ops::rollback(&env.paths, baseline_id).unwrap();
    assert!(rb.restored_files.is_empty());
    assert!(rb.auto_backup_rev_id.is_none());
    assert_eq!(rb.baseline_skipped, vec![cfg.to_str().unwrap().to_string()]);
    assert!(cfg.exists());
    assert_eq!(env.read(&cfg), "export EDITOR=nvim\n");
}

#[test]
fn symlink_awareness_preserves_symlink_structure_on_rollback() {
    let env = TestEnv::new();
    let target_a = env.write("themes/dark.conf", "bg = #000000\n");
    let target_b = env.write("themes/light.conf", "bg = #ffffff\n");
    let link_path = env.work_dir.join("current-theme.conf");
    symlink(&target_a, &link_path).unwrap();

    ops::track(&env.paths, std::slice::from_ref(&link_path)).unwrap();

    // Repoint symlink to target_b and save
    fs::remove_file(&link_path).unwrap();
    symlink(&target_b, &link_path).unwrap();
    let rev_switch = ops::save(&env.paths, std::slice::from_ref(&link_path), "switch theme")
        .unwrap()
        .unwrap()
        .revision_id;

    // Rollback must restore symlink pointing back to target_a without clobbering symlink into a regular file
    ops::rollback(&env.paths, rev_switch).unwrap();

    let meta = fs::symlink_metadata(&link_path).unwrap();
    assert!(meta.file_type().is_symlink(), "symlink structure must be preserved");
    assert_eq!(fs::read_link(&link_path).unwrap(), target_a);
    assert_eq!(env.read(&link_path), "bg = #000000\n");

    // Edit target content in-place through the symlink, save, and roll back
    fs::write(&link_path, "bg = #111111\n").unwrap();
    let rev_edit = ops::save(&env.paths, std::slice::from_ref(&link_path), "tweak dark bg")
        .unwrap()
        .unwrap()
        .revision_id;
    ops::rollback(&env.paths, rev_edit).unwrap();

    assert!(fs::symlink_metadata(&link_path).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_link(&link_path).unwrap(), target_a);
    assert_eq!(env.read(&link_path), "bg = #000000\n");
}

#[test]
fn untrack_preserves_history_and_restore_recovers_specific_revision() {
    let env = TestEnv::new();
    let cfg = env.write("omarchy/look.conf", "font = 'JetBrainsMono'\n");

    let outcomes = ops::track(&env.paths, std::slice::from_ref(&cfg)).unwrap();
    let rev1 = match &outcomes[0] {
        ops::TrackOutcome::Tracked { revision_id, .. } => *revision_id,
        _ => unreachable!(),
    };

    fs::write(&cfg, "font = 'Iosevka'\n").unwrap();
    ops::save(&env.paths, &[], "try Iosevka").unwrap();

    ops::untrack(&env.paths, std::slice::from_ref(&cfg)).unwrap();
    assert!(ops::status(&env.paths).unwrap().is_empty());

    // History remains intact after untrack
    let file_logs = ops::log(&env.paths, Some(&cfg), None).unwrap();
    assert_eq!(file_logs.len(), 2);

    // Restore specific file back to rev1 creates auto-backup and restores content
    let restored = ops::restore(&env.paths, &cfg, rev1).unwrap();
    assert!(restored.auto_backup_rev_id.is_some());
    assert_eq!(env.read(&cfg), "font = 'JetBrainsMono'\n");
}

#[test]
fn cli_end_to_end_json_commands() {
    let root = TempDir::new().unwrap();
    let cfg_dir = root.path().join("cfg");
    let data_dir = root.path().join("data");
    let file = root.path().join("demo.toml");
    fs::write(&file, "k = 1\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_jieun");
    let run = |args: &[&str]| {
        let out = Command::new(bin)
            .args(args)
            .env("JIEUN_CONFIG_DIR", &cfg_dir)
            .env("JIEUN_DATA_DIR", &data_dir)
            .output()
            .expect("run jieun");
        assert!(
            out.status.success(),
            "jieun {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };

    run(&["init"]);
    run(&["init"]); // idempotent
    run(&["save", file.to_str().unwrap(), "-m", "initial via save"]);

    let status_json = run(&["status", "--json"]);
    let status_val: serde_json::Value = serde_json::from_str(&status_json).unwrap();
    assert_eq!(status_val[0]["status"], "CLEAN");

    fs::write(&file, "k = 2\n").unwrap();
    let diff_out = run(&["diff"]);
    assert!(diff_out.contains("-k = 1"));
    assert!(diff_out.contains("+k = 2"));

    run(&["save", "-m", "bump k"]);
    let log_json = run(&["log", "--json", "-n", "1"]);
    let log_val: serde_json::Value = serde_json::from_str(&log_json).unwrap();
    assert_eq!(log_val.as_array().unwrap().len(), 1);
    let latest_id = log_val[0]["id"].as_i64().unwrap();

    let show_json = run(&["show", &latest_id.to_string(), "--json"]);
    let show_val: serde_json::Value = serde_json::from_str(&show_json).unwrap();
    assert_eq!(show_val["message"], "bump k");
    assert!(show_val["files"][0]["diff"].as_str().unwrap().contains("+k = 2"));

    run(&["rollback", &latest_id.to_string(), "--yes"]);
    assert_eq!(fs::read_to_string(&file).unwrap(), "k = 1\n");
}
