//! The native methods of `com.hrvojekatic.dodavo.core.Native`: strings in and
//! out, as `dodavo-ffi` takes and gives them. Each one catches a panic
//! (`with_env`), which must not unwind into Java. The core calls back into
//! the app for one thing: to open a document that the user chose
//! (`com.hrvojekatic.dodavo.core.Documents.open`).

use std::{
    fs::File,
    io,
    os::fd::{FromRawFd, OwnedFd},
    sync::Once,
    time::Duration,
};

use jni::{
    Env, EnvUnowned, JValue,
    errors::{Result, ThrowRuntimeExAndDefault},
    jni_sig, jni_str,
    objects::{JClass, JObject, JString},
    sys::jlong,
};

/// Lets the core open documents through the app (see
/// `dodavo_core::source::set_opener`), once. The class is looked up here, on
/// a thread of the app: one that the core attaches would not find it.
fn install_opener(env: &mut Env<'_>) -> Result<()> {
    static ONCE: Once = Once::new();
    let mut result = Ok(());
    ONCE.call_once(|| {
        result = (|| {
            let vm = env.get_java_vm()?;
            let class = env.find_class(jni_str!("com/hrvojekatic/dodavo/core/Documents"))?;
            let class = env.new_global_ref(class)?;
            dodavo_core::source::set_opener(move |uri| {
                let fd = vm
                    .attach_current_thread(|env| -> Result<i32> {
                        let uri = JString::from_str(env, uri)?;
                        env.call_static_method(
                            &class,
                            jni_str!("open"),
                            jni_sig!("(Ljava/lang/String;)I"),
                            &[JValue::Object(&uri)],
                        )?
                        .i()
                    })
                    .map_err(io::Error::other)?;
                if fd < 0 {
                    // The app says why as an error number (ENOENT, EACCES, ...).
                    return Err(io::Error::from_raw_os_error(-fd));
                }
                // SAFETY: the app detached the descriptor from its own object,
                // so this is its only owner.
                Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
            });
            Ok(())
        })();
    });
    result
}

/// Gives iroh Java's VM and the app's context, once, before the network
/// starts (see `platform::install_android_jni_context`).
fn install_context(env: &mut Env<'_>, context: &JObject<'_>) -> Result<()> {
    static ONCE: Once = Once::new();
    let mut result = Ok(());
    ONCE.call_once(|| {
        result = (|| {
            let vm = env.get_java_vm()?;
            // Kept for the life of the process: iroh holds on to it.
            let context = env.new_global_ref(context)?.into_raw();
            // SAFETY: the VM lives as long as the process does, and the global
            // reference to the app's context is never deleted.
            unsafe {
                dodavo_core::platform::install_android_jni_context(
                    vm.get_raw().cast(),
                    context.cast(),
                );
            }
            Ok(())
        })();
    });
    result
}

/// `start(context: Context, options: String): String`: starts the program
/// with the app's context and the options (see `dodavo_ffi::start`).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_hrvojekatic_dodavo_core_Native_start<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
    options: JString<'local>,
) -> JString<'local> {
    env.with_env(|env| -> Result<JString<'local>> {
        install_context(env, &context)?;
        install_opener(env)?;
        let options = options.try_to_string(env)?;
        JString::from_str(env, dodavo_ffi::start(&options, None))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `call(call: String): String` (see `dodavo_ffi::call`).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_hrvojekatic_dodavo_core_Native_call<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    call: JString<'local>,
) -> JString<'local> {
    env.with_env(|env| -> Result<JString<'local>> {
        let call = call.try_to_string(env)?;
        JString::from_str(env, dodavo_ffi::call(&call))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `poll(): String?`: the next message, or null (see `dodavo_ffi::poll`).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_hrvojekatic_dodavo_core_Native_poll<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> JString<'local> {
    env.with_env(|env| -> Result<JString<'local>> {
        match dodavo_ffi::poll() {
            Some(message) => JString::from_str(env, message),
            None => Ok(JString::default()),
        }
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `next(timeoutMs: Long): String?`: the next message, waiting for one at
/// most `timeoutMs`, or null (see `dodavo_ffi::wait`). For a thread of the
/// app that does nothing else.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_hrvojekatic_dodavo_core_Native_next<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    timeout_ms: jlong,
) -> JString<'local> {
    env.with_env(|env| -> Result<JString<'local>> {
        let timeout = Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(0));
        match dodavo_ffi::wait(timeout) {
            Some(message) => JString::from_str(env, message),
            None => Ok(JString::default()),
        }
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
