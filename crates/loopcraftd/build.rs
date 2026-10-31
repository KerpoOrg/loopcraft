//! Embeds Info.plist into the binary, so macOS can name the daemon in its
//! privacy prompts (audio capture) and remember the permission.

fn main() {
    let plist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Info.plist");
    println!("cargo:rerun-if-changed={}", plist.display());
    println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
}
