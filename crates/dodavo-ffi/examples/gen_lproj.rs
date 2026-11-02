//! Writes the `.lproj` folders of the app (`macos/build.sh`): what macOS shows
//! in each language, and which languages the app has at all, on which the
//! language of its panels depends.
//!
//!     cargo run -p dodavo-ffi --example gen_lproj -- <Resources folder>

use std::path::Path;

use dodavo_core::i18n::{Lang, mac};

/// A string in the `.strings` format.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn main() {
    let dest = std::env::args().nth(1).expect("the Resources folder");
    for lang in Lang::ALL {
        let table = mac::strings(lang);
        let folder = Path::new(&dest).join(format!("{}.lproj", lang.code()));
        std::fs::create_dir_all(&folder).unwrap();
        let info = [
            ("NSLocalNetworkUsageDescription", table.ns_local_network),
            ("NSDownloadsFolderUsageDescription", table.ns_downloads),
            ("NSDocumentsFolderUsageDescription", table.ns_documents),
            ("NSDesktopFolderUsageDescription", table.ns_desktop),
            ("NSRemovableVolumesUsageDescription", table.ns_removable),
            ("NSNetworkVolumesUsageDescription", table.ns_network),
        ];
        let text: String = info
            .iter()
            .map(|(key, value)| format!("{key} = {};\n", quoted(value)))
            .collect();
        std::fs::write(folder.join("InfoPlist.strings"), text).unwrap();
        std::fs::write(
            folder.join("ServicesMenu.strings"),
            format!(
                "{} = {};\n",
                quoted("Send with Dodavo"),
                quoted(table.service_send)
            ),
        )
        .unwrap();
    }
}
