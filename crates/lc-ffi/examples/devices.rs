//! Prints what the editor gets for each audio device: `cargo run -p lc-ffi --example devices`.
fn main() {
    let p = lc_ffi::lc_devices();
    let s = unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { lc_ffi::lc_free(p) };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    println!("{}", serde_json::to_string_pretty(&v).unwrap());
}
