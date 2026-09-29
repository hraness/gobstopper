// Windows gives a program's main thread 1 MiB of stack, and the command-line
// parser for gobstopper's command tree needs more than that in debug builds.
// Reserve 8 MiB, the Linux and macOS default, so the binary behaves the same
// everywhere.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
}
