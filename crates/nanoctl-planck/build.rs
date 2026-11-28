fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/windows.rc");
        println!("cargo:rerun-if-changed=assets/nanoctl.ico");
        embed_resource::compile("assets/windows.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
