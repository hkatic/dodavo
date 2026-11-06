//! Writes Android's string resources from the Android texts (see
//! `dodavo::strings_xml`):
//!
//!     cargo run -p dodavo-android --example gen_strings -- android/app/src/main/res

use std::path::Path;

use dodavo::strings_xml::{FOLDERS, strings_xml};

fn main() {
    let res = std::env::args().nth(1).expect("the res folder of the app");
    for (folder, lang) in FOLDERS {
        let dir = Path::new(&res).join(folder);
        std::fs::create_dir_all(&dir).expect("the folder");
        std::fs::write(dir.join("strings.xml"), strings_xml(lang)).expect("the file");
    }
}
