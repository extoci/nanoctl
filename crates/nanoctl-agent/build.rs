use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn collect(root: &Path, path: &Path, entries: &mut Vec<String>) {
    if let Ok(items) = fs::read_dir(path) {
        for item in items.flatten() {
            let path = item.path();
            if path.is_dir() {
                collect(root, &path, entries);
            } else if path.is_file() {
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                entries.push(format!(
                    "({:?}, include_bytes!({:?}) as &'static [u8]),",
                    format!("/{name}"),
                    path
                ));
            }
        }
    }
}

fn main() {
    #[cfg(feature = "x264")]
    build_x264();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
        && env::var_os("CARGO_FEATURE_MEDIA").is_some()
    {
        println!("cargo:rerun-if-changed=src/nvenc.c");
        println!("cargo:rustc-link-lib=dl");
        cc::Build::new()
            .file("src/nvenc.c")
            .std("c11")
            .compile("nanoctl_nvenc");
    }

    let root =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../apps/viewer/dist");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut entries = Vec::new();
    collect(&root, &root, &mut entries);
    entries.sort();
    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("viewer.rs"),
        format!(
            "pub static ASSETS: &[(&str, &[u8])] = &[{}];",
            entries.join("\n")
        ),
    )
    .unwrap();
}

#[cfg(feature = "x264")]
fn build_x264() {
    let library = pkg_config::Config::new()
        .cargo_metadata(false)
        .statik(false)
        .probe("x264")
        .expect("feature x264 requires system libx264 headers and pkg-config (libx264-dev)");
    let mut build = cc::Build::new();
    build.file("src/x264_shim.c");
    for include in library.include_paths {
        build.include(include);
    }
    build.compile("nanoctl_x264_shim");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=dl");
    }
    println!("cargo:rerun-if-changed=src/x264_shim.c");
}
