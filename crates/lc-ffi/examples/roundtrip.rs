//! Checks that a config survives the editor's model unchanged (writes nothing).
//!   cargo run -p lc-ffi --example roundtrip [PATH]
fn main() {
    let path = std::env::args().nth(1).map(std::path::PathBuf::from).unwrap_or_else(lc_config::default_path);
    let config = lc_config::load(&path).unwrap_or_else(|e| panic!("{e}"));
    let editor = lc_ffi::to_editor(&config);
    let json = serde_json::to_string(&editor).unwrap();
    let back: lc_ffi::EConfig = serde_json::from_str(&json).unwrap();
    let yaml = lc_ffi::render(&back).unwrap_or_else(|e| panic!("{e}"));
    let reparsed = lc_config::parse(&yaml).unwrap();
    assert_eq!(reparsed, config, "the editor would change the configuration");
    println!("{}: survives the editor unchanged ({} bytes of YAML)", path.display(), yaml.len());
}
