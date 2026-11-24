use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use jieun::cli::{Cli, Commands};
use jieun::config::AppPaths;
use jieun::ops::{self, FileState, TrackOutcome, UntrackOutcome};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = AppPaths::resolve()?;
    let use_color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    match cli.command {
        Commands::Init => {
            ops::init(&paths)?;
            println!("Initialized jieun:");
            println!("  config:  {}", paths.config_dir.display());
            println!("  data:    {}", paths.data_dir.display());
            println!("  db:      {}", paths.db_path().display());
            println!("  objects: {}", paths.objects_dir().display());
        }

        Commands::Track { paths: target_paths } => {
            for outcome in ops::track(&paths, &target_paths)? {
                match outcome {
                    TrackOutcome::Tracked { path, revision_id } => {
                        println!("Tracked {path} (baseline rev #{revision_id})");
                    }
                    TrackOutcome::Reactivated { path } => {
                        println!("Reactivated tracking for {path}");
                    }
                    TrackOutcome::AlreadyTracked { path } => {
                        println!("Already tracked: {path}");
                    }
                }
            }
        }

        Commands::Untrack {
            paths: target_paths,
            yes,
        } => {
            if !yes
                && !confirm(&format!(
                    "Untrack {} file(s)? Revision history will be preserved.",
                    target_paths.len()
                ))?
            {
                println!("Aborted.");
                return Ok(());
            }

            for outcome in ops::untrack(&paths, &target_paths)? {
                match outcome {
                    UntrackOutcome::Untracked { path } => {
                        println!("Untracked {path} (history preserved)");
                    }
                    UntrackOutcome::NotTracked { path } => {
                        println!("Not actively tracked: {path}");
                    }
                }
            }
        }

        Commands::Status => {
            let entries = ops::status(&paths)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
                return Ok(());
            }
            if entries.is_empty() {
                println!("No actively tracked files. Use 'jieun track <path>' to start.");
                return Ok(());
            }
            for entry in entries {
                let (label, color) = match entry.status {
                    FileState::Clean => ("CLEAN   ", GREEN),
                    FileState::Modified => ("MODIFIED", YELLOW),
                    FileState::Missing => ("MISSING ", RED),
                };
                let symlink_tag = if entry.is_symlink { " [symlink]" } else { "" };
                if use_color {
                    println!("{color}{label}{RESET}  {}{symlink_tag}", entry.path);
                } else {
                    println!("{label}  {}{symlink_tag}", entry.path);
                }
            }
        }

        Commands::Diff { path } => {
            let entries = ops::diff(&paths, path.as_deref(), use_color)?;
            if entries.is_empty() {
                println!("No changes.");
                return Ok(());
            }
            for entry in entries {
                if entry.missing {
                    eprintln!(
                        "warning: '{}' is missing on disk (last snapshot rev #{})",
                        entry.path, entry.last_revision_id
                    );
                    continue;
                }
                if let Some(note) = &entry.symlink_change {
                    println!("# {}: {note}", entry.path);
                }
                if !entry.diff_text.is_empty() {
                    print!("{}", entry.diff_text);
                }
            }
        }

        Commands::Save {
            paths: target_paths,
            message,
        } => match ops::save(&paths, &target_paths, &message)? {
            None => {
                println!("No changes to save.");
            }
            Some(res) => {
                println!(
                    "Saved rev #{} ({} file(s)): {}",
                    res.revision_id,
                    res.saved_files.len(),
                    message.trim()
                );
                for f in &res.saved_files {
                    println!("  + {f}");
                }
                for f in &res.skipped_missing {
                    eprintln!("warning: skipped missing tracked file '{f}'");
                }
            }
        },

        Commands::Log { path, limit } => {
            let entries = ops::log(&paths, path.as_deref(), limit)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
                return Ok(());
            }
            if entries.is_empty() {
                println!("No revisions recorded yet.");
                return Ok(());
            }
            for entry in entries {
                let badge = if entry.is_auto_backup {
                    " [auto-backup]"
                } else {
                    ""
                };
                if use_color {
                    println!(
                        "{CYAN}#{:<4}{RESET} {DIM}{}{RESET}  {BOLD}{}{RESET}{YELLOW}{badge}{RESET}",
                        entry.id, entry.timestamp, entry.message
                    );
                } else {
                    println!(
                        "#{:<4} {}  {}{badge}",
                        entry.id, entry.timestamp, entry.message
                    );
                }
                for f in &entry.files {
                    println!("      {f}");
                }
            }
        }

        Commands::Show { rev_id } => {
            let detail = ops::show(&paths, rev_id, use_color && !cli.json)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&detail)?);
                return Ok(());
            }
            let badge = if detail.is_auto_backup {
                " [auto-backup]"
            } else {
                ""
            };
            println!("Revision #{}{badge}", detail.id);
            println!("Date:     {}", detail.timestamp);
            println!("Message:  {}", detail.message);
            println!("Files:    {}", detail.files.len());
            for f in &detail.files {
                println!();
                let sym = match &f.symlink_target {
                    Some(t) if f.is_symlink => format!(" (symlink -> {t})"),
                    _ => String::new(),
                };
                println!("File: {} [mode {}]{sym}", f.file_path, f.file_mode);
                println!(
                    "Hash: {} -> {}",
                    f.prev_hash
                        .as_deref()
                        .map(short_hash)
                        .unwrap_or("<baseline>"),
                    short_hash(&f.new_hash)
                );
                if !f.diff.is_empty() {
                    print!("{}", f.diff);
                } else {
                    println!("(no textual diff)");
                }
            }
        }

        Commands::Rollback { rev_id, yes } => {
            let plan = ops::plan_rollback(&paths, rev_id)?;

            for skipped in &plan.baseline_skipped {
                println!(
                    "Info: '{skipped}' was initially tracked in rev #{rev_id} (no previous version; file will not be deleted)."
                );
            }

            if plan.targets.is_empty() {
                println!("Nothing to roll back for rev #{rev_id}.");
                return Ok(());
            }

            for warn in &plan.warnings {
                if use_color {
                    eprintln!("{YELLOW}WARNING:{RESET} {warn}");
                } else {
                    eprintln!("WARNING: {warn}");
                }
            }

            println!("Will roll back rev #{} (\"{}\"):", plan.rev_id, plan.message);
            for t in &plan.targets {
                println!("  <- {} (to {})", t.file_path, short_hash(&t.target_hash));
            }

            if !yes && !confirm("Proceed with rollback? An auto-backup will be created first.")? {
                println!("Aborted.");
                return Ok(());
            }

            let res = ops::execute_rollback(&paths, &plan)?;
            if let Some(backup_id) = res.auto_backup_rev_id {
                println!("Created auto-backup rev #{backup_id}");
            }
            if let Some(rb_id) = res.rollback_rev_id {
                println!(
                    "Rolled back rev #{} (recorded as rev #{}, {} file(s) restored)",
                    plan.rev_id,
                    rb_id,
                    res.restored_files.len()
                );
            }
        }

        Commands::Restore { path, to, yes } => {
            let plan = ops::plan_restore(&paths, &path, to)?;

            if let Some(warn) = &plan.warning {
                if use_color {
                    eprintln!("{YELLOW}WARNING:{RESET} {warn}");
                } else {
                    eprintln!("WARNING: {warn}");
                }
            }

            println!(
                "Will restore '{}' to rev #{} ({})",
                plan.file_path,
                plan.rev_id,
                short_hash(&plan.target_hash)
            );

            if !yes && !confirm("Proceed with restore? An auto-backup will be created first.")? {
                println!("Aborted.");
                return Ok(());
            }

            let res = ops::execute_restore(&paths, &plan)?;
            if let Some(backup_id) = res.auto_backup_rev_id {
                println!("Created auto-backup rev #{backup_id}");
            }
            println!(
                "Restored '{}' to rev #{} (recorded as rev #{})",
                res.file_path, plan.rev_id, res.restore_rev_id
            );
        }
    }

    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N]: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

fn short_hash(hash: &str) -> &str {
    &hash[..hash.len().min(12)]
}
