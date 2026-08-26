fn main() {
    // Use an absolute path derived from the manifest directory rather
    // than a relative one: the linker's working directory is not
    // guaranteed to be the workspace root.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{manifest_dir}/kernel.ld");
    // Without a dynamic loader there is nothing to ever make the GOT
    // read-only again, so `-z relro` (the linker's default) buys nothing
    // here. Worse, it costs correctness: it carves the tiny `.got` a
    // linker inserts for RIP-relative statics into its own PT_LOAD
    // segment, immediately after `.rodata` and *not* page-aligned — so it
    // lands on the same page `.rodata` already occupies. Our loader
    // allocates each PT_LOAD segment's pages independently
    // (`elf.rs::load_segment`, `AllocateType::Address`), so the second
    // attempt to claim that shared page fails outright
    // (`ElfError::AllocationFailed`) before the kernel ever runs.
    // `-z norelro` folds `.got` into `.data`, which the linker script
    // already page-aligns, removing the overlap.
    println!("cargo:rustc-link-arg=-znorelro");
    println!("cargo:rerun-if-changed=kernel.ld");
    println!("cargo:rerun-if-changed=build.rs");
}
