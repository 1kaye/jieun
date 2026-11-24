use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};

const ZSTD_LEVEL: i32 = 3;
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub hash: String,
    pub bytes: Vec<u8>,
    pub mode: u32,
    pub is_symlink: bool,
    pub symlink_target: Option<String>,
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Inspect a file on disk using `symlink_metadata` so symlinks are detected accurately.
/// Returns `Ok(None)` if the path does not exist on disk.
pub fn inspect_file(path: &Path) -> Result<Option<FileSnapshot>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };

    if meta.file_type().is_symlink() {
        let target = fs::read_link(path)
            .with_context(|| format!("failed to read symlink target for {}", path.display()))?;
        let target_str = target.to_string_lossy().into_owned();

        let (bytes, mode) = match fs::metadata(path) {
            Ok(target_meta) => {
                if target_meta.is_dir() {
                    bail!(
                        "cannot track symlink pointing to a directory: {}",
                        path.display()
                    );
                }
                let bytes = fs::read(path).with_context(|| {
                    format!("failed to read symlink target content for {}", path.display())
                })?;
                (bytes, target_meta.permissions().mode() & 0o7777)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => (target_str.as_bytes().to_vec(), 0o777),
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("failed to stat symlink target for {}", path.display())
                });
            }
        };

        let hash = hash_bytes(&bytes);
        return Ok(Some(FileSnapshot {
            hash,
            bytes,
            mode,
            is_symlink: true,
            symlink_target: Some(target_str),
        }));
    }

    if !meta.is_file() {
        bail!(
            "cannot track '{}' (only regular files and file symlinks are supported)",
            path.display()
        );
    }

    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mode = meta.permissions().mode() & 0o7777;
    let hash = hash_bytes(&bytes);

    Ok(Some(FileSnapshot {
        hash,
        bytes,
        mode,
        is_symlink: false,
        symlink_target: None,
    }))
}

pub fn blob_path(objects_dir: &Path, hash: &str) -> Result<PathBuf> {
    if hash.len() < 2 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid object hash: '{hash}'");
    }
    Ok(objects_dir.join(&hash[..2]).join(format!("{hash}.zst")))
}

pub fn write_blob(objects_dir: &Path, hash: &str, bytes: &[u8]) -> Result<()> {
    let dest = blob_path(objects_dir, hash)?;
    if dest.exists() {
        return Ok(());
    }

    let compressed = zstd::stream::encode_all(bytes, ZSTD_LEVEL)
        .context("failed to compress blob with zstd")?;
    atomic_write(&dest, &compressed, 0o644)
        .with_context(|| format!("failed to write CAS object {hash}"))
}

pub fn read_blob(objects_dir: &Path, hash: &str) -> Result<Vec<u8>> {
    let src = blob_path(objects_dir, hash)?;
    let file = File::open(&src)
        .with_context(|| format!("missing CAS object '{}' at {}", hash, src.display()))?;
    let raw = zstd::stream::decode_all(file)
        .with_context(|| format!("failed to decompress CAS object '{hash}'"))?;

    let actual = hash_bytes(&raw);
    if actual != hash {
        bail!("corrupted CAS object '{hash}': expected {hash}, computed {actual}");
    }
    Ok(raw)
}

/// Write bytes atomically to `dest` via a sibling `.jieun.tmp.<random>` file,
/// applying exact Unix permissions and calling `sync_all()` before `rename()`.
pub fn atomic_write(dest: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow!("target path has no parent directory: {}", dest.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create parent directory {}", parent.display()))?;

    let (tmp_path, mut tmp_file) = create_unique_temp_file(parent)?;

    let write_res = (|| -> Result<()> {
        tmp_file.write_all(bytes)?;
        tmp_file.set_permissions(Permissions::from_mode(mode & 0o7777))?;
        tmp_file.sync_all()?;
        Ok(())
    })();

    drop(tmp_file);

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err).with_context(|| format!("failed writing temp file for {}", dest.display()));
    }

    if let Err(err) = fs::rename(&tmp_path, dest) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err).with_context(|| format!("failed to atomically replace {}", dest.display()));
    }

    Ok(())
}

/// Restore either a regular file or a symlink atomically while preserving symlink structure
/// and Unix file permissions.
pub fn restore_file_atomically(
    objects_dir: &Path,
    dest: &Path,
    hash: &str,
    mode: u32,
    is_symlink: bool,
    symlink_target: Option<&str>,
) -> Result<()> {
    if !is_symlink {
        let bytes = read_blob(objects_dir, hash)?;
        return atomic_write(dest, &bytes, mode);
    }

    let target = symlink_target
        .ok_or_else(|| anyhow!("symlink record for {} is missing target", dest.display()))?;
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow!("target path has no parent directory: {}", dest.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create parent directory {}", parent.display()))?;

    let needs_relink = match fs::symlink_metadata(dest) {
        Ok(meta) if meta.file_type().is_symlink() => fs::read_link(dest)
            .map(|existing| existing.to_string_lossy() != target)
            .unwrap_or(true),
        _ => true,
    };

    if needs_relink {
        atomic_symlink(parent, dest, target)?;
    }

    let resolved_target = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        parent.join(target)
    };

    if let Ok(target_meta) = fs::metadata(&resolved_target)
        && target_meta.is_file()
    {
        let current_bytes = fs::read(&resolved_target).with_context(|| {
            format!(
                "failed to read symlink target {}",
                resolved_target.display()
            )
        })?;
        if hash_bytes(&current_bytes) != hash {
            let blob = read_blob(objects_dir, hash)?;
            atomic_write(&resolved_target, &blob, mode)?;
        }
    }

    Ok(())
}

fn atomic_symlink(parent: &Path, dest: &Path, target: &str) -> Result<()> {
    for _ in 0..32 {
        let tmp_link = parent.join(unique_tmp_name());
        match symlink(target, &tmp_link) {
            Ok(()) => {
                if let Err(err) = fs::rename(&tmp_link, dest) {
                    let _ = fs::remove_file(&tmp_link);
                    return Err(err).with_context(|| {
                        format!("failed to atomically replace symlink {}", dest.display())
                    });
                }
                return Ok(());
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("failed to create temp symlink in {}", parent.display())
                });
            }
        }
    }
    bail!("exhausted retries creating temp symlink in {}", parent.display())
}

fn create_unique_temp_file(parent: &Path) -> Result<(PathBuf, File)> {
    for _ in 0..32 {
        let tmp_path = parent.join(unique_tmp_name());
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(file) => return Ok((tmp_path, file)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("failed to create temp file in {}", parent.display())
                });
            }
        }
    }
    bail!("exhausted retries creating temp file in {}", parent.display())
}

fn unique_tmp_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let seed = format!("{pid}-{nanos}-{seq}");
    let suffix = &blake3::hash(seed.as_bytes()).to_hex()[..12];
    format!(".jieun.tmp.{suffix}")
}
