//! Writes the Swift enum of the text keys, so that a key that does not exist
//! fails to compile there as it does here. `macos/build.sh` runs it; a test
//! checks that the committed file is current.

#[cfg(target_os = "macos")]
fn main() {
    print!("{}", dodavo_ffi::swift_keys());
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("The Swift keys are made on a Mac.");
}
