# The native methods are looked up by their names (crates/dodavo-android).
-keep class com.hrvojekatic.dodavo.core.Native {
    native <methods>;
}

# The core opens documents through this (crates/dodavo-android, jni_api.rs).
-keep class com.hrvojekatic.dodavo.core.Documents {
    public static int open(java.lang.String);
}
