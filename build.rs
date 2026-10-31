fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.manifest");
    println!("cargo:rerun-if-changed=res/app.ico");
    // The version resource comes from Cargo.toml, so the program, the
    // installer (installer\Dodavo.iss) and Settings → Apps show the same one.
    let env = |name| std::env::var(name).unwrap();
    let (major, minor, patch) = (
        env("CARGO_PKG_VERSION_MAJOR"),
        env("CARGO_PKG_VERSION_MINOR"),
        env("CARGO_PKG_VERSION_PATCH"),
    );
    // The application manifest cannot take macros, so its version is written
    // by hand; make sure it was changed together with Cargo.toml.
    let manifest = std::fs::read_to_string("res/app.manifest").unwrap();
    let expected = format!("version=\"{major}.{minor}.{patch}.0\"");
    assert!(
        manifest.contains(&expected),
        "res/app.manifest: the assemblyIdentity version must be {expected}, as in Cargo.toml"
    );
    let macros = [
        format!("VER_MAJOR={major}"),
        format!("VER_MINOR={minor}"),
        format!("VER_PATCH={patch}"),
        format!("VER_STR=\"{}\"", env("CARGO_PKG_VERSION")),
    ];
    embed_resource::compile("res/app.rc", embed_resource::ParamsMacros(&macros))
        .manifest_required()
        .unwrap();
}
