use std::fs;

use obsidian_to_logseq::convert::{self, Conversion, ConvertOptions};
use obsidian_to_logseq::vault::Vault;

/// Converts a temporary vault made of `(path, content)` files.
fn convert_vault(files: &[(&str, &str)]) -> (Conversion, String) {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let p = dir.path().join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
    }
    let mut vault = Vault::scan(dir.path(), &[]).unwrap();
    vault.load_daily_config(None, None).unwrap();
    let options = ConvertOptions {
        nest_under_headings: true,
        keep_comments: false,
        tags_as_pages: false,
        journals: true,
        reserved_titles: Default::default(),
    };
    let conversion = convert::convert(&vault, &options).unwrap();
    let edn = conversion.edn.to_edn_string();
    (conversion, edn)
}

#[test]
fn non_finite_and_duplicate_numbers_are_dropped() {
    let (_, edn) = convert_vault(&[(
        "A.md",
        "---\nscore: .nan\nvals: [1, 1.0, .nan, .nan, 2.5]\n---\nbody",
    )]);
    assert!(!edn.contains("nil"), "{edn}");
    assert!(!edn.contains(":user.property/score"), "{edn}");
    assert!(edn.contains(":user.property/vals #{1 2.5}"), "{edn}");
}

#[test]
fn empty_url_and_node_values_are_dropped() {
    let (_, edn) = convert_vault(&[
        (
            "A.md",
            "---\nsite: https://example.com\nrel: \"[[C]]\"\n---\na",
        ),
        ("B.md", "---\nsite: \"\"\nrel: \"\"\n---\nb"),
        ("C.md", "c"),
    ]);
    assert_eq!(edn.matches(":user.property/site \"").count(), 1, "{edn}");
    assert_eq!(edn.matches(":user.property/rel [").count(), 1, "{edn}");
}

#[test]
fn duplicate_frontmatter_keys_keep_the_rest() {
    let (conv, edn) = convert_vault(&[(
        "A.md",
        "---\nrating: 1\nrating: 2\nposition: Manager\ntags: [proj]\n---\nbody",
    )]);
    assert!(edn.contains(":user.property/rating 2"), "{edn}");
    assert!(edn.contains("\"Manager\""), "{edn}");
    assert!(edn.contains(":block/title \"proj\""), "{edn}");
    assert!(
        conv.warnings
            .iter()
            .any(|w| w.contains("\"rating\" repeated")),
        "{:?}",
        conv.warnings
    );
}
