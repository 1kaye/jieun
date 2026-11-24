use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "jieun",
    version,
    about = "File-scoped configuration Version Control System for Linux"
)]
pub struct Cli {
    /// Emit structured JSON output (supported by status, log, show)
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Initialize jieun XDG directories and SQLite database (idempotent)
    Init,

    /// Register one or more files for tracking and record their initial baseline
    Track {
        /// File paths to track
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },

    /// Stop actively tracking files without deleting their revision history
    Untrack {
        /// File paths to untrack
        #[arg(required = true)]
        paths: Vec<PathBuf>,

        /// Skip interactive confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Show status (CLEAN, MODIFIED, MISSING) of all actively tracked files
    Status,

    /// Show colored unified diff of working file(s) against their latest snapshot
    Diff {
        /// Optional specific file path to diff (defaults to all MODIFIED tracked files)
        path: Option<PathBuf>,
    },

    /// Snapshot changed file(s) into a new isolated revision
    Save {
        /// Optional file paths to track & snapshot (defaults to all MODIFIED tracked files)
        paths: Vec<PathBuf>,

        /// Revision message describing the change
        #[arg(short = 'm', long = "message")]
        message: String,
    },

    /// List recent revisions, optionally filtered to a specific file
    Log {
        /// Optional file path to filter revision history
        path: Option<PathBuf>,

        /// Maximum number of revisions to display
        #[arg(short = 'n', long = "limit")]
        limit: Option<usize>,
    },

    /// Show detailed metadata and unified diff for a revision
    Show {
        /// Revision ID to inspect
        rev_id: i64,
    },

    /// Roll back only the files modified in a specific revision to their previous state
    Rollback {
        /// Revision ID to roll back
        rev_id: i64,

        /// Skip interactive confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Restore a specific file to its recorded state at a given revision
    Restore {
        /// File path to restore
        path: PathBuf,

        /// Target revision ID to restore from
        #[arg(long = "to")]
        to: i64,

        /// Skip interactive confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
}
