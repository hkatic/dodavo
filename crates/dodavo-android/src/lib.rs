//! The library that Android's app (`android/`, in Kotlin) loads as
//! `libdodavo.so`: the functions of `dodavo-ffi` for Java ([`jni_api`]), and
//! the Android resources made from the Android texts ([`strings_xml`]).

#[cfg(target_os = "android")]
mod jni_api;
pub mod strings_xml;
