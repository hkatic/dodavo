//! Release tool: makes the signing key and the update manifest that the
//! program checks (see `src/update/`). Built only with the `release-tool`
//! feature; `installer\publish.ps1` runs it.
//!
//!     dodavo-release keygen --out <dir>
//!     dodavo-release manifest --installer <setup.exe> --key <update-key.secret>
//!                             --notes <dir> --out <update.json>
//!                             [--version <v>] [--url <url>] [--published <YYYY-MM-DD>]
//!     dodavo-release verify --manifest <update.json> --installer <setup.exe> [--public <hex>]
//!     dodavo-release statement --installer <setup.exe> --version <v>
//!
//! The shared files are compiled in directly, so this tool and the program
//! always agree on the manifest format, the statement and the trusted keys.

#[path = "../update/keys.rs"]
mod keys;
#[allow(dead_code)]
#[path = "../update/manifest.rs"]
mod manifest;
#[path = "../update/statement.rs"]
mod statement;
#[allow(dead_code)]
#[path = "../update/version.rs"]
mod version;

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    process::ExitCode,
};

use ed25519_dalek::{SigningKey, VerifyingKey};
use manifest::{Installer, Manifest};
use version::Version;

const REPO: &str = "hkatic/dodavo";
const LANGS: [&str; 4] = ["en", "hr", "sr-Latn", "sl"];
const SECRET_FILE: &str = "update-key.secret";
const PUBLIC_FILE: &str = "update-key.public";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let options = match parse_options(rest) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match command.as_str() {
        "keygen" => keygen(&options),
        "manifest" => make_manifest(&options),
        "verify" => verify(&options),
        "statement" => print_statement(&options),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
Usage:
  dodavo-release keygen --out <dir>
      Writes update-key.secret and update-key.public (hex) into <dir>.
  dodavo-release manifest --installer <setup.exe> --key <update-key.secret>
                          --notes <dir> --out <update.json>
                          [--version <v>] [--url <url>] [--published <YYYY-MM-DD>]
      Hashes and signs the installer and writes the update manifest. The
      version comes from the file name Dodavo-<v>-setup.exe unless given;
      <dir> holds en.txt (required), hr.txt, sr-Latn.txt and sl.txt.
  dodavo-release verify --manifest <update.json> --installer <setup.exe> [--public <hex>]
      Checks the manifest against the installer and the keys built into
      Dodavo (or the given public key).
  dodavo-release statement --installer <setup.exe> --version <v>
      Prints the text that gets signed.";

type Options = HashMap<String, String>;

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = HashMap::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let Some(name) = arg.strip_prefix("--") else {
            return Err(format!("unexpected argument {arg:?}"));
        };
        let value = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
        options.insert(name.to_string(), value.clone());
    }
    Ok(options)
}

fn required<'a>(options: &'a Options, name: &str) -> Result<&'a str, String> {
    options
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| format!("--{name} is required"))
}

fn keygen(options: &Options) -> Result<(), String> {
    let dir = PathBuf::from(required(options, "out")?);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let secret_path = dir.join(SECRET_FILE);
    let public_path = dir.join(PUBLIC_FILE);
    if secret_path.exists() {
        return Err(format!(
            "{} exists already; not overwriting a signing key",
            secret_path.display()
        ));
    }
    let secret: [u8; 32] = rand::random();
    let public = SigningKey::from_bytes(&secret).verifying_key();
    let public_hex = manifest::to_hex(&public.to_bytes());
    std::fs::write(&secret_path, format!("{}\n", manifest::to_hex(&secret)))
        .map_err(|e| format!("{}: {e}", secret_path.display()))?;
    std::fs::write(&public_path, format!("{public_hex}\n"))
        .map_err(|e| format!("{}: {e}", public_path.display()))?;
    println!("Secret key written to {}", secret_path.display());
    println!("Public key written to {}", public_path.display());
    println!();
    println!("Public key (put it into src/update/keys.rs):");
    println!("{public_hex}");
    println!();
    println!(
        "Keep {SECRET_FILE} private and back it up: without it no further update can be signed."
    );
    Ok(())
}

fn read_secret(path: &Path) -> Result<[u8; 32], String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    manifest::from_hex(text.trim())
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| format!("{} does not hold 64 hex digits", path.display()))
}

/// `Dodavo-<version>-setup.exe` → `<version>`.
fn version_from_file_name(installer: &Path) -> Option<String> {
    let name = installer.file_name()?.to_str()?;
    let version = name.strip_prefix("Dodavo-")?.strip_suffix("-setup.exe")?;
    Version::parse(version).map(|_| version.to_string())
}

fn read_notes(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut notes = BTreeMap::new();
    for lang in LANGS {
        let path = dir.join(format!("{lang}.txt"));
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let text = text
                    .trim_start_matches('\u{feff}')
                    .replace("\r\n", "\n")
                    .trim_end()
                    .to_string();
                if text.is_empty() {
                    return Err(format!("{} is empty", path.display()));
                }
                notes.insert(lang.to_string(), text);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if lang == manifest::FALLBACK_LANG {
                    return Err(format!("{} is required", path.display()));
                }
                eprintln!(
                    "warning: {} is missing; {lang} users will read the English notes",
                    path.display()
                );
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(notes)
}

fn make_manifest(options: &Options) -> Result<(), String> {
    let installer = PathBuf::from(required(options, "installer")?);
    let key = read_secret(Path::new(required(options, "key")?))?;
    let notes = read_notes(Path::new(required(options, "notes")?))?;
    let out = PathBuf::from(required(options, "out")?);
    let from_name = version_from_file_name(&installer);
    let version = match (options.get("version"), from_name) {
        (Some(given), Some(name)) if *given != name => {
            return Err(format!(
                "--version {given} does not match the installer name ({name})"
            ));
        }
        (Some(given), _) => given.clone(),
        (None, Some(name)) => name,
        (None, None) => {
            return Err(
                "give --version; the installer is not named Dodavo-<version>-setup.exe".into(),
            );
        }
    };
    if Version::parse(&version).is_none() {
        return Err(format!("{version:?} is not a valid version"));
    }
    let file_name = installer
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("the installer path has no file name")?;
    let url = options.get("url").cloned().unwrap_or_else(|| {
        format!("https://github.com/{REPO}/releases/download/v{version}/{file_name}")
    });
    let size = std::fs::metadata(&installer)
        .map_err(|e| format!("{}: {e}", installer.display()))?
        .len();
    let sha256 = manifest::to_hex(
        &statement::sha256_file(&installer).map_err(|e| format!("{}: {e}", installer.display()))?,
    );
    let text = statement::statement(manifest::FORMAT, &version, &sha256, size);
    let manifest = Manifest {
        format: manifest::FORMAT,
        version: version.clone(),
        published: options.get("published").cloned().or_else(|| Some(today())),
        installer: Installer { url, size, sha256 },
        signature: statement::sign(&key, &text),
        notes,
    };
    manifest.validate()?;
    let public = SigningKey::from_bytes(&key).verifying_key();
    if !statement::verify(&[public], &manifest) {
        return Err("the fresh signature does not verify (this is a bug)".into());
    }
    let json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(&out, format!("{json}\n")).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("Wrote {}", out.display());
    println!("  version:   {version}");
    println!("  installer: {} ({size} bytes)", manifest.installer.url);
    println!("  sha256:    {}", manifest.installer.sha256);
    println!(
        "  notes:     {}",
        manifest
            .notes
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    let built_in = built_in_keys();
    if !statement::verify(&built_in, &manifest) {
        eprintln!(
            "warning: the program's built-in keys (src/update/keys.rs) do not include this key; \
             installed copies will reject this manifest"
        );
    }
    Ok(())
}

fn built_in_keys() -> Vec<VerifyingKey> {
    keys::PUBLIC_KEYS_HEX
        .iter()
        .filter_map(|hex| statement::parse_public_key(hex))
        .collect()
}

fn verify(options: &Options) -> Result<(), String> {
    let manifest_path = PathBuf::from(required(options, "manifest")?);
    let installer = PathBuf::from(required(options, "installer")?);
    let bytes =
        std::fs::read(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let manifest = manifest::parse(&bytes)?;
    let keys = match options.get("public") {
        Some(hex) => vec![statement::parse_public_key(hex).ok_or("--public is not a valid key")?],
        None => built_in_keys(),
    };
    if !statement::verify(&keys, &manifest) {
        return Err("the signature does not verify against the trusted keys".into());
    }
    let size = std::fs::metadata(&installer)
        .map_err(|e| format!("{}: {e}", installer.display()))?
        .len();
    if size != manifest.installer.size {
        return Err(format!(
            "size mismatch: manifest says {}, file has {size}",
            manifest.installer.size
        ));
    }
    let sha256 = manifest::to_hex(
        &statement::sha256_file(&installer).map_err(|e| format!("{}: {e}", installer.display()))?,
    );
    if sha256 != manifest.installer.sha256 {
        return Err(format!(
            "sha256 mismatch: manifest says {}, file has {sha256}",
            manifest.installer.sha256
        ));
    }
    if let Some(expected) = version_from_file_name(&installer)
        && expected != manifest.version
    {
        return Err(format!(
            "version mismatch: manifest says {}, installer is {expected}",
            manifest.version
        ));
    }
    println!(
        "OK: {} is the signed installer of Dodavo {} ({} bytes)",
        installer.display(),
        manifest.version,
        size
    );
    Ok(())
}

fn print_statement(options: &Options) -> Result<(), String> {
    let installer = PathBuf::from(required(options, "installer")?);
    let version = required(options, "version")?;
    let size = std::fs::metadata(&installer)
        .map_err(|e| format!("{}: {e}", installer.display()))?
        .len();
    let sha256 = manifest::to_hex(
        &statement::sha256_file(&installer).map_err(|e| format!("{}: {e}", installer.display()))?,
    );
    print!(
        "{}",
        statement::statement(manifest::FORMAT, version, &sha256, size)
    );
    Ok(())
}

/// Today's date as `YYYY-MM-DD` (UTC), without a date crate.
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
