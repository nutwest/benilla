//! Stamps the commit this binary was built from, and gives the main thread the stack the app needs,
//! as the `benilla` launcher's build script does.

fn main() {
    benilla_buildstamp::emit();
    // The login screen overflows the 1 MB the MSVC linker reserves for the main thread by default.
    const STACK: u32 = 8 << 20;
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:{STACK}"),
        Ok("gnu") => println!("cargo:rustc-link-arg-bins=-Wl,--stack,{STACK}"),
        _ => {}
    }
}
