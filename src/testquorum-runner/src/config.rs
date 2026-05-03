use std::path::Path;
use std::path::PathBuf;

const CONFIG_PATHS: &[&str] = &[
    ".github/testquorum.toml",
    ".testquorum.toml",
    "testquorum.toml",
];

pub(crate) fn find_config_file() -> Option<PathBuf> {
    CONFIG_PATHS
        .iter()
        .map(Path::new)
        .find(|p| p.exists())
        .map(|p| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_paths_cover_documented_locations() {
        assert!(CONFIG_PATHS.contains(&".github/testquorum.toml"));
        assert!(CONFIG_PATHS.contains(&".testquorum.toml"));
        assert!(CONFIG_PATHS.contains(&"testquorum.toml"));
    }
}
