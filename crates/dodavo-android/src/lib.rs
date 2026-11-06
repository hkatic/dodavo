//! The library that Android's app (`android/`, in Kotlin) loads as
//! `libdodavo.so`: the functions of `dodavo-ffi` for Java ([`jni_api`]), and
//! the Android resources made from the Android texts ([`strings_xml`]). The
//! tests of the help pages are here too (`help`).

#[cfg(test)]
mod help;
#[cfg(target_os = "android")]
mod jni_api;
pub mod strings_xml;
