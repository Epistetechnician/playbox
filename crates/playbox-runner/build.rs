//! With the `krun` feature, find libkrun so the helper binary can link it.
use std::{env, path::Path};

fn main() {
    println!("cargo:rerun-if-env-changed=LIBKRUN_LIB_DIR");
    if env::var_os("CARGO_FEATURE_KRUN").is_none() {
        return;
    }
    let mut dirs: Vec<String> = env::var("LIBKRUN_LIB_DIR").into_iter().collect();
    dirs.extend(
        ["/opt/homebrew/lib", "/usr/local/lib", "/usr/local/lib64", "/usr/lib64", "/usr/lib/aarch64-linux-gnu", "/usr/lib/x86_64-linux-gnu"]
            .map(String::from),
    );
    let found = dirs.iter().find(|d| ["libkrun.dylib", "libkrun.so"].iter().any(|f| Path::new(d).join(f).exists()));
    match found {
        Some(d) => {
            println!("cargo:rustc-link-search=native={d}");
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{d}");
        }
        None => panic!(
            "libkrun not found. Install it (macOS: `brew tap libkrun/krun && brew install libkrun`) or set LIBKRUN_LIB_DIR."
        ),
    }
}
