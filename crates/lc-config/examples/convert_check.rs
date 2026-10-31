//! Converts a config to the id form and checks nothing the daemon sees changes:
//! `convert_check <config.yaml>` prints the converted file.
fn main() {
    let path = std::env::args().nth(1).expect("config path");
    let old = lc_config::load(std::path::Path::new(&path)).unwrap();
    let yaml = lc_config::to_yaml(&old);
    let new = lc_config::parse(&yaml).unwrap();
    assert_eq!(new, old, "same mixer after conversion");
    assert_eq!(new.to_specs(), old.to_specs(), "same devices for macOS");
    eprintln!("ok: identical mixer, wires and devices");
    print!("{yaml}");
}
