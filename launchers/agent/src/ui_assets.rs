//! The committed UI build, embedded at compile time by build.rs.
include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

/// An embedded `public/` asset by its flat file name.
pub(crate) fn get(name: &str) -> Option<&'static [u8]> {
    ASSETS
        .iter()
        .find(|(asset, _)| *asset == name)
        .map(|(_, bytes)| *bytes)
}

#[cfg(test)]
mod tests {
    #[test]
    fn core_ui_entry_points_are_embedded() {
        for name in ["index.html", "boot.js", "ui.js", "ui.css", "ui-assets.json"] {
            assert!(
                super::get(name).is_some_and(|bytes| !bytes.is_empty()),
                "{name}"
            );
        }
        assert!(super::get("../Cargo.toml").is_none());
    }
}
