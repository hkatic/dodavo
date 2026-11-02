//! Writes the Swift enum of the text keys, so that a key that does not exist
//! fails to compile there as it does here. `macos/build.sh` runs it; a test
//! checks that the committed file is current.

fn main() {
    print!("{}", dodavo_ffi::swift_keys());
}
