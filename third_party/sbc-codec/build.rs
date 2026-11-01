use std::{env, path::PathBuf, process::Command};

fn run(mut command: Command) {
    let status = command
        .status()
        .expect("run SBC compiler (install Clang and LLVM ar)");
    assert!(status.success(), "SBC compiler failed: {command:?}");
}

fn main() {
    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=AR");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let compiler = env::var("CC").unwrap_or_else(|_| "clang".into());
    let archiver = env::var("AR").unwrap_or_else(|_| "llvm-ar".into());
    let mut objects = Vec::new();
    for source in ["sbc", "sbc_primitives", "canopus_sbc_wrapper"] {
        let object = out.join(format!("{source}.o"));
        let mut cmd = Command::new(&compiler);
        if target.starts_with("wasm32") {
            // Freestanding objects need only Clang's builtin headers; the Rust
            // WASI link supplies memcpy/memset. No external WASI SDK/sysroot.
            cmd.arg("--target=wasm32-unknown-unknown");
        }
        cmd.args([
            "-O3",
            "-ffreestanding",
            "-fno-common",
            "-fno-builtin",
            "-fdata-sections",
            "-ffunction-sections",
            "-DCANOPUS_FREESTANDING=1",
            "-D__BYTE_ORDER=__ORDER_LITTLE_ENDIAN__",
            "-D__LITTLE_ENDIAN=__ORDER_LITTLE_ENDIAN__",
            "-D__BIG_ENDIAN=__ORDER_BIG_ENDIAN__",
            "-Wno-unused-parameter",
            "-Wno-ignored-attributes",
            "-Ivendor",
            "-c",
        ])
        .arg(format!("vendor/{source}.c"))
        .arg("-o")
        .arg(&object);
        run(cmd);
        objects.push(object);
    }
    let mut archive = Command::new(archiver);
    archive
        .arg("crs")
        .arg(out.join("libsbc_client.a"))
        .args(objects);
    run(archive);
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=sbc_client");
}
