use std::{env, fmt, fs, io, path::PathBuf};

/// Errors returned while preparing application storage.
#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    InvalidComponent(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "storage I/O failed: {error}"),
            Self::InvalidComponent(component) => {
                write!(formatter, "invalid storage path component: {component:?}")
            }
        }
    }
}

impl std::error::Error for StorageError {}

impl From<io::Error> for StorageError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Paths for the application's cache and configuration data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachePaths {
    root: PathBuf,
}

impl CachePaths {
    /// Chooses the platform cache directory and appends a sanitized app name.
    pub fn for_app(app_name: &str) -> Result<Self, StorageError> {
        let app_name = safe_component(app_name)?;
        let base = platform_cache_root();
        Ok(Self {
            root: base.join(app_name),
        })
    }

    /// Creates paths under an explicit root. Useful for tests and portable mode.
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    pub fn tiles_root(&self) -> PathBuf {
        self.root.join("tiles")
    }

    pub fn tile_namespace(&self, namespace: &str) -> Result<PathBuf, StorageError> {
        Ok(self.tiles_root().join(safe_component(namespace)?))
    }

    pub fn ensure(&self) -> Result<(), StorageError> {
        fs::create_dir_all(self.tiles_root())?;
        Ok(())
    }
}

fn platform_cache_root() -> PathBuf {
    if cfg!(target_os = "windows")
        && let Some(local_app_data) = env::var_os("LOCALAPPDATA")
    {
        return PathBuf::from(local_app_data);
    }

    if let Some(cache_home) = env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache_home);
    }

    if let Some(home) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")) {
        return PathBuf::from(home).join(".cache");
    }

    env::temp_dir()
}

fn safe_component(component: &str) -> Result<String, StorageError> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.contains(['/', '\\'])
    {
        return Err(StorageError::InvalidComponent(component.to_owned()));
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(component.len());
    for byte in component.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            encoded.push(byte as char);
        } else {
            // `~` is not emitted literally, so escaped bytes cannot collide
            // with a namespace that already contains a sequence such as
            // `~20`.
            encoded.push('~');
            encoded.push(HEX[usize::from(byte >> 4)] as char);
            encoded.push(HEX[usize::from(byte & 0x0f)] as char);
        }
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_root_has_stable_layout() {
        let paths = CachePaths::from_root("test-cache");
        assert_eq!(
            paths.tiles_root(),
            PathBuf::from("test-cache").join("tiles")
        );
        assert_eq!(
            paths.tile_namespace("osm").expect("valid namespace"),
            PathBuf::from("test-cache").join("tiles").join("osm")
        );
    }

    #[test]
    fn path_traversal_is_rejected() {
        let paths = CachePaths::from_root("test-cache");
        assert!(paths.tile_namespace("../outside").is_err());
        assert!(CachePaths::for_app("").is_err());
    }

    #[test]
    fn unusual_names_are_sanitized() {
        let paths = CachePaths::from_root("test-cache");
        assert_eq!(
            paths.tile_namespace("provider name").expect("sanitized"),
            PathBuf::from("test-cache/tiles/provider~20name")
        );
    }

    #[test]
    fn unusual_names_do_not_collide() {
        let paths = CachePaths::from_root("test-cache");
        let first = paths.tile_namespace("a b").expect("valid namespace");
        let second = paths.tile_namespace("a?b").expect("valid namespace");
        assert_ne!(first, second);
    }
}
