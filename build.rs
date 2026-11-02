//! Embeds res/app.rc (the icon, the application manifest and the version
//! resource) with the version, the author, the website and the years of the
//! copyright from Cargo.toml, so the program, Setup (installer\Dodavo.iss
//! reads them from the program) and Settings → Apps show the same ones.

use std::{env, fs, path::Path};

fn main() {
    for file in [
        "Cargo.toml",
        "res/app.rc",
        "res/app.manifest",
        "res/app.ico",
    ] {
        println!("cargo:rerun-if-changed={file}");
    }
    let var = |name| env::var(name).unwrap();
    let years = copyright_years(&fs::read_to_string("Cargo.toml").unwrap());
    // ui::COPYRIGHT_YEARS.
    println!("cargo:rustc-env=DODAVO_COPYRIGHT_YEARS={years}");
    // Everything else here is for Windows' version resource.
    if var("CARGO_CFG_TARGET_OS") != "windows" {
        return;
    }
    let out_dir = var("OUT_DIR");
    let out_dir = Path::new(&out_dir);
    let (major, minor, patch) = (
        var("CARGO_PKG_VERSION_MAJOR"),
        var("CARGO_PKG_VERSION_MINOR"),
        var("CARGO_PKG_VERSION_PATCH"),
    );
    // Several authors are separated by colons.
    let author = var("CARGO_PKG_AUTHORS").replace(':', ", ");
    let website = var("CARGO_PKG_REPOSITORY");

    // The application manifest cannot take macros: res/app.manifest is a
    // template whose @VERSION@ is filled in here. rc.exe finds the result in
    // OUT_DIR, which embed-resource passes as an include directory.
    let manifest = fs::read_to_string("res/app.manifest").unwrap();
    assert!(
        manifest.contains("@VERSION@"),
        "res/app.manifest: the assemblyIdentity version must be @VERSION@"
    );
    let manifest = manifest.replace("@VERSION@", &format!("{major}.{minor}.{patch}.0"));
    fs::write(out_dir.join("Dodavo.exe.manifest"), manifest).unwrap();

    // The values res/app.rc includes, in UTF-8 like app.rc itself.
    let header = [
        format!("#define VER_MAJOR {major}"),
        format!("#define VER_MINOR {minor}"),
        format!("#define VER_PATCH {patch}"),
        format!("#define VER_STR {}", rc_string(&var("CARGO_PKG_VERSION"))),
        format!("#define APP_AUTHOR {}", rc_string(&author)),
        format!(
            "#define APP_COPYRIGHT {}",
            rc_string(&format!("Copyright © {years} {author}"))
        ),
        format!("#define APP_WEBSITE {}", rc_string(&website)),
    ];
    fs::write(out_dir.join("app-info.h"), header.join("\n") + "\n").unwrap();

    embed_resource::compile("res/app.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}

/// A string literal for rc.exe.
fn rc_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\"\""))
}

/// `copyright-years` from the `[package.metadata.dodavo]` table of Cargo.toml.
fn copyright_years(cargo_toml: &str) -> String {
    let mut table = "";
    for line in cargo_toml.lines().map(str::trim) {
        if line.starts_with('[') {
            table = line;
        } else if table == "[package.metadata.dodavo]"
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "copyright-years"
        {
            let value = value.trim();
            if let Some(years) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                return years.to_string();
            }
        }
    }
    panic!("Cargo.toml: [package.metadata.dodavo] needs copyright-years = \"…\"");
}
