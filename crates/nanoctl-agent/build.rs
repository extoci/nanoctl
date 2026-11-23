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
    if env::var_os("CARGO_FEATURE_EMBEDDED_TAILNET").is_some() {
        build_tsnet();
    }
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

fn build_tsnet() {
    let sources = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../nanoctl-tsnet");
    for file in ["main.go", "go.mod", "go.sum"] {
        println!("cargo:rerun-if-changed={}", sources.join(file).display());
    }
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let go_os = match os.as_str() {
        "windows" | "linux" => os.as_str(),
        "macos" => "darwin",
        _ => panic!("embedded-tailnet does not support {os}"),
    };
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let go_arch = match arch.as_str() {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => panic!("embedded-tailnet does not support {arch}"),
    };
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let go = || {
        let mut cmd = std::process::Command::new("go");
        cmd.current_dir(&sources)
            .env("GOOS", go_os)
            .env("GOARCH", go_arch)
            .env("CGO_ENABLED", "0");
        cmd
    };
    let status = go()
        .args([
            "build",
            "-mod=readonly",
            "-trimpath",
            "-buildvcs=false",
            "-ldflags=-s -w",
            "-o",
        ])
        .arg(out_dir.join("nanoctl-tsnet"))
        .arg(".")
        .status()
        .expect("embedded-tailnet requires Go (see crates/nanoctl-tsnet/go.mod)");
    assert!(
        status.success(),
        "cannot build the embedded Tailscale helper"
    );

    let deps = go()
        .args([
            "list",
            "-mod=readonly",
            "-deps",
            "-f",
            "{{with .Module}}{{if .Version}}{{.Path}} {{.Version}}\t{{.Dir}}{{end}}{{end}}",
            ".",
        ])
        .output()
        .expect("cannot list embedded Tailscale dependencies");
    assert!(
        deps.status.success(),
        "cannot list embedded Tailscale dependencies"
    );
    let modules: std::collections::BTreeSet<_> = std::str::from_utf8(&deps.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect();
    let goroot = std::process::Command::new("go")
        .args(["env", "GOROOT"])
        .output()
        .unwrap();
    assert!(
        goroot.status.success(),
        "cannot locate the Go standard library license"
    );
    let mut notices = String::from("Go standard library\n\n");
    notices.push_str(
        &fs::read_to_string(
            Path::new(std::str::from_utf8(&goroot.stdout).unwrap().trim()).join("LICENSE"),
        )
        .unwrap(),
    );
    for (module, dir) in modules {
        use std::fmt::Write;
        let _ = writeln!(notices, "\n\n{module}\n");
        for file in [
            "LICENSE",
            "LICENSE.txt",
            "LICENSE.md",
            "COPYING",
            "NOTICE",
            "NOTICE.txt",
        ] {
            if let Ok(text) = fs::read_to_string(Path::new(dir).join(file)) {
                let _ = writeln!(notices, "{text}");
            }
        }
    }
    fs::write(out_dir.join("tsnet-notices.txt"), notices).unwrap();
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
