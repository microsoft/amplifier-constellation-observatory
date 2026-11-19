use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=native/macos_picker.m");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let architecture = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        _ => panic!("Unsupported Mac helper architecture"),
    };
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    let object = output.join("macos_picker.o");
    let library = output.join("libconstellation_picker.a");
    assert!(
        Command::new("/usr/bin/xcrun")
            .args([
                "--sdk",
                "macosx",
                "clang",
                "-arch",
                architecture,
                "-mmacosx-version-min=11.0",
                "-fobjc-arc",
                "-c",
                "native/macos_picker.m",
                "-o"
            ])
            .arg(&object)
            .status()
            .expect("Mac system compiler")
            .success(),
        "Native file picker compilation failed"
    );
    assert!(
        Command::new("/usr/bin/xcrun")
            .args(["--sdk", "macosx", "ar", "rcs"])
            .arg(&library)
            .arg(&object)
            .status()
            .expect("Mac system archiver")
            .success(),
        "Native file picker archive failed"
    );
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-lib=static=constellation_picker");
    println!("cargo:rustc-link-lib=dylib=objc");
    for framework in ["AppKit", "Foundation", "UniformTypeIdentifiers"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
