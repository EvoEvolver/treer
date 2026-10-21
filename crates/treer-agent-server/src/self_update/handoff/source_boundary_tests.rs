//! Source audit tripwire, not a Rust parser or a substitute for privacy checks.
//! Private fields in the leaf module provide the actual compiler boundary.
use std::path::Path;

#[test]
fn launcher_frame_is_constructed_only_in_its_authenticated_leaf_reader() {
    fn visit(directory: &Path, literals: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                visit(&entry.path(), literals);
            } else if kind.is_file() && entry.path().extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(entry.path()).unwrap();
                let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
                // Assemble the needle so this source check does not count its
                // own string literal as an unauthorized construction site.
                let needle = ["Launcher", "Frame", "{"].concat();
                for _ in compact.match_indices(&needle) {
                    literals.push(entry.path());
                }
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/self_update");
    let mut literals = Vec::new();
    visit(&root, &mut literals);
    assert_eq!(literals, [root.join("handoff/launcher_frame.rs")]);

    // Keep the defining module a leaf. Introducing child modules would extend
    // the private-field construction boundary and needs an explicit review.
    let leaf = include_str!("launcher_frame.rs");
    assert!(!leaf.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("mod ") || line.starts_with("pub mod ")
    }));
}
