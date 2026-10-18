#[test]
fn the_readme_pins_the_crate_version() {
    let readme = include_str!("../README.md");
    let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    let pins: Vec<&str> = readme
        .match_indices("--tag v")
        .map(|(i, _)| &readme[i + 6..])
        .chain(
            readme
                .match_indices("tag = \"v")
                .map(|(i, _)| &readme[i + 7..]),
        )
        .map(|rest| {
            rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
                .next()
                .unwrap_or_default()
        })
        .collect();
    assert!(!pins.is_empty());
    for pin in pins {
        assert_eq!(pin, tag, "README pins {pin}; Cargo.toml is {tag}");
    }
}
