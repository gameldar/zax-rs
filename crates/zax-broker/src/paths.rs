//! Where the broker's socket, lock and log live.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerPaths {
    pub socket: PathBuf,
    pub lock: PathBuf,
    pub log: PathBuf,
}

impl BrokerPaths {
    pub fn in_dir(dir: &Path, log_dir: &Path) -> Self {
        Self {
            socket: dir.join("broker.sock"),
            lock: dir.join("broker.lock"),
            log: log_dir.join("broker.log"),
        }
    }

    /// `$XDG_RUNTIME_DIR/zax`, else the cache directory, for the socket and
    /// lock; the cache directory for the log. The cache directory is
    /// `$XDG_CACHE_HOME/zax`, else `~/.cache/zax`, matching zax's tmux.conf.
    pub fn resolve() -> Result<Self, String> {
        let cache = match std::env::var_os("XDG_CACHE_HOME") {
            Some(v) if !v.is_empty() => PathBuf::from(v).join("zax"),
            _ => std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".cache").join("zax"))
                .ok_or("HOME is not set")?,
        };
        let runtime = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(v) if !v.is_empty() => PathBuf::from(v).join("zax"),
            _ => cache.clone(),
        };
        Ok(Self::in_dir(&runtime, &cache))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_lock_sits_beside_the_socket() {
        let p = super::BrokerPaths::in_dir(
            std::path::Path::new("/run/zax"),
            std::path::Path::new("/c/zax"),
        );
        assert_eq!(p.socket, std::path::PathBuf::from("/run/zax/broker.sock"));
        assert_eq!(p.lock, std::path::PathBuf::from("/run/zax/broker.lock"));
        assert_eq!(p.log, std::path::PathBuf::from("/c/zax/broker.log"));
    }
}
