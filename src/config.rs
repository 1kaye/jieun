use std::env;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use directories::BaseDirs;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl AppPaths {
    pub fn resolve() -> Result<Self> {
        let base = BaseDirs::new();

        let config_dir = match env::var_os("JIEUN_CONFIG_DIR").filter(|v| !v.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => base
                .as_ref()
                .map(|b| b.config_dir().join("jieun"))
                .ok_or_else(|| anyhow!("unable to determine XDG config directory"))?,
        };

        let data_dir = match env::var_os("JIEUN_DATA_DIR").filter(|v| !v.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => base
                .as_ref()
                .map(|b| b.data_local_dir().join("jieun"))
                .ok_or_else(|| anyhow!("unable to determine XDG data directory"))?,
        };

        Ok(Self {
            config_dir,
            data_dir,
        })
    }

    pub fn from_dirs(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            config_dir,
            data_dir,
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("history.db")
    }

    pub fn objects_dir(&self) -> PathBuf {
        self.data_dir.join("objects")
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
}

/// Expand leading `~` and normalize relative / `.` / `..` segments lexically
/// without dereferencing symlinks so symlink tracking and missing-file queries stay accurate.
pub fn normalize_path(input: &Path) -> Result<PathBuf> {
    let expanded = expand_tilde(input)?;
    let abs = if expanded.is_absolute() {
        expanded
    } else {
        env::current_dir()
            .context("failed to determine current working directory")?
            .join(expanded)
    };

    let mut normalized = PathBuf::new();
    for component in abs.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized.file_name().is_some() {
                    normalized.pop();
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    Ok(normalized)
}

pub fn path_to_string(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("path contains invalid UTF-8: {}", path.display()))
}

fn expand_tilde(path: &Path) -> Result<PathBuf> {
    let mut components = path.components();
    if let Some(Component::Normal(first)) = components.next()
        && first == "~"
    {
        let home = home_dir()?;
        let rest: PathBuf = components.collect();
        return Ok(if rest.as_os_str().is_empty() {
            home
        } else {
            home.join(rest)
        });
    }
    Ok(path.to_path_buf())
}

fn home_dir() -> Result<PathBuf> {
    if let Some(home) = env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .ok_or_else(|| anyhow!("unable to determine home directory for '~' expansion"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_dot_segments_without_touching_disk() {
        let p = normalize_path(Path::new("/tmp/a/./b/../c/nonexistent.conf")).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/a/c/nonexistent.conf"));
    }

    #[test]
    fn expands_tilde_prefix() {
        let p = normalize_path(Path::new("~/.config/herdr/../jieun/config.toml")).unwrap();
        assert!(p.is_absolute());
        assert!(p.ends_with(".config/jieun/config.toml"));
    }
}
