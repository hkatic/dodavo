fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.manifest");
    println!("cargo:rerun-if-changed=res/app.ico");
    // The version resource comes from Cargo.toml, so the program, the
    // installer (installer\Dodavo.iss) and Settings → Apps show the same one.
    let env = |name| std::env::var(name).unwrap();
    let macros = [
        format!("VER_MAJOR={}", env("CARGO_PKG_VERSION_MAJOR")),
        format!("VER_MINOR={}", env("CARGO_PKG_VERSION_MINOR")),
        format!("VER_PATCH={}", env("CARGO_PKG_VERSION_PATCH")),
        format!("VER_STR=\"{}\"", env("CARGO_PKG_VERSION")),
    ];
    embed_resource::compile("res/app.rc", embed_resource::ParamsMacros(&macros))
        .manifest_required()
        .unwrap();
}
