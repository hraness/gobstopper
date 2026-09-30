// Windows gives a program's main thread 1 MiB of stack, and the command-line
// parser for gobstopper's command tree needs more than that in debug builds.
// Reserve 8 MiB, the Linux and macOS default, so the binary behaves the same
// everywhere.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GOBSTOPPER_BUILD_RELEASE_TAG");
    if let Ok(tag) = std::env::var("GOBSTOPPER_BUILD_RELEASE_TAG") {
        let version = std::env::var("CARGO_PKG_VERSION").expect("Cargo package version");
        assert_eq!(
            tag,
            format!("v{version}"),
            "release build tag must match the package version"
        );
        println!("cargo:rustc-env=GOBSTOPPER_COMPILED_RELEASE_TAG={tag}");
    } else {
        println!("cargo:rustc-env=GOBSTOPPER_COMPILED_RELEASE_TAG=");
    }
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
}
