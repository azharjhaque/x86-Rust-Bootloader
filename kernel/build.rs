fn main() {
    // Use an absolute path derived from the manifest directory rather
    // than a relative one: the linker's working directory is not
    // guaranteed to be the workspace root.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{manifest_dir}/kernel.ld");
    println!("cargo:rerun-if-changed=kernel.ld");
    println!("cargo:rerun-if-changed=build.rs");
}
