//! Local copies of Lua mode packages.
//!
//! The server is authoritative; players only download the builder package
//! (manifest + Lua source) so they have it on disk next to their own modes.
//! Nothing here influences gameplay — the client never executes mode scripts.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::protocol::message::ModePackage;

/// `$LAN_BASH_MODES_DIR` or `./modes`.
pub fn modes_dir() -> PathBuf {
    std::env::var_os("LAN_BASH_MODES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("modes"))
}

/// Writes `modes/<id>/mode.json` and `modes/<id>/script.lua` below the
/// configured modes directory.
pub fn store(package: &ModePackage) -> Result<PathBuf> {
    store_in(&modes_dir(), package)
}

/// [`store`] with an explicit root (the env var is process-global, which makes
/// tests order-dependent).
///
/// The id must be a plain slug: a downloaded package is untrusted input, so it
/// may never escape the modes directory. A copy that differs from the local one
/// is kept as a sibling `<id>_server` folder so it never overwrites a mode the
/// player is editing in the builder.
pub fn store_in(root: &Path, package: &ModePackage) -> Result<PathBuf> {
    if !is_valid_id(&package.id) {
        anyhow::bail!("refusing to store mode with unsafe id {:?}", package.id);
    }

    let mut folder = package.id.clone();
    let mut dir = root.join(&folder);

    if dir.join("script.lua").is_file() {
        let local = std::fs::read_to_string(dir.join("script.lua")).unwrap_or_default();

        if local != package.script {
            folder = format!("{}_server", package.id);
            dir = root.join(&folder);
        }
    }

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating mode folder {}", dir.display()))?;

    let mut manifest = package.manifest.clone();

    if let Some(object) = manifest.as_object_mut() {
        object.insert("id".to_owned(), serde_json::json!(folder));
    }

    let manifest = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(dir.join("mode.json"), format!("{manifest}\n"))
        .with_context(|| format!("writing {}", dir.join("mode.json").display()))?;
    std::fs::write(dir.join("script.lua"), &package.script)
        .with_context(|| format!("writing {}", dir.join("script.lua").display()))?;

    Ok(dir)
}

/// Same rule as the server and the builder.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal_ids() {
        for id in [
            "../evil",
            "a/b",
            "",
            "Upper",
            "sp ace",
            "..",
            "x".repeat(65).as_str(),
        ] {
            assert!(!is_valid_id(id), "{id:?} must be rejected");
        }
        assert!(is_valid_id("coin_rush"));
        assert!(is_valid_id("tag-2"));
    }

    #[test]
    fn stores_a_package_in_a_mode_folder() {
        let temp = tempfile::tempdir().unwrap();

        let package = ModePackage {
            id: "coin_rush".to_owned(),
            name: "Coin Rush".to_owned(),
            manifest: serde_json::json!({"id": "coin_rush", "world": {"width": 960, "height": 540}}),
            script: "function on_start() end".to_owned(),
            checksum: "abc".to_owned(),
        };

        let dir = store_in(temp.path(), &package).expect("store");
        assert_eq!(dir, temp.path().join("coin_rush"));
        assert!(dir.join("mode.json").is_file());
        assert_eq!(
            std::fs::read_to_string(dir.join("script.lua")).unwrap(),
            "function on_start() end"
        );
    }

    #[test]
    fn a_different_local_script_is_not_overwritten() {
        let temp = tempfile::tempdir().unwrap();

        let mut package = ModePackage {
            id: "coin_rush".to_owned(),
            name: "Coin Rush".to_owned(),
            manifest: serde_json::json!({"id": "coin_rush"}),
            script: "local mine = true".to_owned(),
            checksum: "local".to_owned(),
        };

        store_in(temp.path(), &package).expect("first copy");
        package.script = "local server = true".to_owned();
        let dir = store_in(temp.path(), &package).expect("second copy");

        assert_eq!(dir, temp.path().join("coin_rush_server"));
        assert_eq!(
            std::fs::read_to_string(temp.path().join("coin_rush/script.lua")).unwrap(),
            "local mine = true"
        );
    }
}
