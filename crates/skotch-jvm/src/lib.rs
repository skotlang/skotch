//! In-process JVM via the JNI Invocation API.
//!
//! [`EmbeddedJvm`] wraps `jni::JavaVM` and adds:
//!
//! - **Automatic libjvm discovery** across OS/JDK layouts
//! - **Class-from-bytes loading** via JNI `DefineClass`
//! - **stdout capture** by temporarily redirecting `System.out`
//! - **Defensive error messages** listing every probed path on failure
//!
//! The JNI spec allows only one `JavaVM` per process.
//! `EmbeddedJvm::new` enforces this with a global `Once`.

pub mod locate;

use anyhow::{anyhow, Result};
use jni::objects::{JByteArray, JObject, JValue};
use jni::{InitArgsBuilder, JNIVersion, JavaVM};
use std::sync::{Mutex, Once};

/// `Once` ensures `create_jvm` runs EXACTLY once. The result is
/// leaked into `&'static JavaVM` so all threads can share it.
static JVM_INIT: Once = Once::new();
static JVM_REF: Mutex<Option<&'static JavaVM>> = Mutex::new(None);
static JVM_ERR: Mutex<Option<String>> = Mutex::new(None);

/// Mutex protecting `System.setOut` during `run_class_main`.
static STDOUT_LOCK: Mutex<()> = Mutex::new(());

/// An in-process JVM created via `JNI_CreateJavaVM`.
pub struct EmbeddedJvm {
    _private: (),
}

impl EmbeddedJvm {
    /// Create (or reuse) the in-process JVM singleton.
    pub fn new() -> Result<Self> {
        JVM_INIT.call_once(|| match Self::create_jvm() {
            Ok(jvm) => {
                // Leak into 'static so all threads can share a
                // reference without a Mutex on the hot path.
                let leaked: &'static JavaVM = Box::leak(Box::new(jvm));
                *JVM_REF.lock().unwrap() = Some(leaked);
            }
            Err(e) => {
                *JVM_ERR.lock().unwrap() = Some(format!("{e:#}"));
            }
        });
        if let Some(e) = JVM_ERR.lock().unwrap().as_ref() {
            return Err(anyhow!("failed to create JVM: {e}"));
        }
        Ok(EmbeddedJvm { _private: () })
    }

    fn create_jvm() -> Result<JavaVM> {
        let libjvm_path = locate::find_libjvm()?;

        // Ensure the JVM's sibling libraries (libjava, etc.) are
        // findable at runtime. On Windows the JVM handles this
        // internally via PATH, so we only need to set the library
        // search path on Unix platforms.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(dir) = libjvm_path.parent() {
            let dir_str = dir.to_string_lossy();
            #[cfg(target_os = "macos")]
            if !std::env::var("DYLD_LIBRARY_PATH")
                .unwrap_or_default()
                .contains(&*dir_str)
            {
                let cur = std::env::var("DYLD_LIBRARY_PATH").unwrap_or_default();
                std::env::set_var(
                    "DYLD_LIBRARY_PATH",
                    if cur.is_empty() {
                        dir_str.to_string()
                    } else {
                        format!("{dir_str}:{cur}")
                    },
                );
            }
            #[cfg(target_os = "linux")]
            if !std::env::var("LD_LIBRARY_PATH")
                .unwrap_or_default()
                .contains(&*dir_str)
            {
                let cur = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
                std::env::set_var(
                    "LD_LIBRARY_PATH",
                    if cur.is_empty() {
                        dir_str.to_string()
                    } else {
                        format!("{dir_str}:{cur}")
                    },
                );
            }
        }

        let jvm_args = InitArgsBuilder::new()
            .version(JNIVersion::V8)
            .build()
            .map_err(|e| anyhow!("JVM InitArgs build failed: {e}"))?;

        let jvm = JavaVM::new(jvm_args).map_err(|e| {
            anyhow!(
                "JNI_CreateJavaVM failed: {e}\n  libjvm: {}",
                libjvm_path.display()
            )
        })?;
        Ok(jvm)
    }

    /// Get the global JVM reference.
    pub fn jvm() -> &'static JavaVM {
        JVM_REF
            .lock()
            .unwrap()
            .expect("JVM not initialized — call EmbeddedJvm::new() first")
    }

    /// Define a class from `.class` bytes and call its
    /// `public static void main(String[])`.
    /// Returns the captured stdout as a String.
    pub fn run_class_main(&self, class_name: &str, class_bytes: &[u8]) -> Result<String> {
        // Serialize all run_class_main calls so the System.out
        // redirect is atomic (tests run in parallel).
        let _guard = STDOUT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let env = Self::jvm()
            .attach_current_thread()
            .map_err(|e| anyhow!("JNI attach failed: {e}"))?;
        // JNIEnv through the attach guard implements Deref<Target=JNIEnv>.
        // We need a mutable reference for all JNI calls.
        // jni 0.21 returns AttachGuard which impls DerefMut.
        let mut env = env;

        // ── Redirect System.out ──────────────────────────────────
        let baos_class = env
            .find_class("java/io/ByteArrayOutputStream")
            .map_err(|e| anyhow!("FindClass ByteArrayOutputStream: {e}"))?;
        let baos = env
            .new_object(baos_class, "()V", &[])
            .map_err(|e| anyhow!("new ByteArrayOutputStream: {e}"))?;
        let ps_class = env
            .find_class("java/io/PrintStream")
            .map_err(|e| anyhow!("FindClass PrintStream: {e}"))?;
        let ps = env
            .new_object(
                ps_class,
                "(Ljava/io/OutputStream;)V",
                &[JValue::Object(&baos)],
            )
            .map_err(|e| anyhow!("new PrintStream: {e}"))?;

        let system_class = env
            .find_class("java/lang/System")
            .map_err(|e| anyhow!("FindClass System: {e}"))?;

        // Save old System.out via System.out getter
        let old_out = env
            .get_static_field(&system_class, "out", "Ljava/io/PrintStream;")
            .map_err(|e| anyhow!("get System.out: {e}"))?
            .l()
            .map_err(|e| anyhow!("System.out as Object: {e}"))?;

        // Use System.setOut(ps) — the official API that updates
        // both the field and any internal JVM caches.
        env.call_static_method(
            &system_class,
            "setOut",
            "(Ljava/io/PrintStream;)V",
            &[JValue::Object(&ps)],
        )
        .map_err(|e| anyhow!("System.setOut: {e}"))?;

        // ── Define class and call main ───────────────────────────
        let run_result = self.define_and_run(&mut env, class_name, class_bytes);

        // ── Restore System.out (always) via System.setOut ────────
        env.call_static_method(
            &system_class,
            "setOut",
            "(Ljava/io/PrintStream;)V",
            &[JValue::Object(&old_out)],
        )
        .ok(); // best-effort restore

        // Check for JVM exceptions
        if env.exception_check().unwrap_or(false) {
            env.exception_clear().ok();
        }
        run_result?;

        // ── Read captured stdout ─────────────────────────────────
        let bytes_obj = env
            .call_method(&baos, "toByteArray", "()[B", &[])
            .map_err(|e| anyhow!("toByteArray: {e}"))?
            .l()
            .map_err(|e| anyhow!("toByteArray result: {e}"))?;
        let byte_array = JByteArray::from(bytes_obj);
        let len = env
            .get_array_length(&byte_array)
            .map_err(|e| anyhow!("array length: {e}"))? as usize;
        let mut buf = vec![0i8; len];
        env.get_byte_array_region(&byte_array, 0, &mut buf)
            .map_err(|e| anyhow!("get_byte_array_region: {e}"))?;
        let bytes: Vec<u8> = buf.into_iter().map(|b| b as u8).collect();
        String::from_utf8(bytes).map_err(|e| anyhow!("stdout not UTF-8: {e}"))
    }

    /// Define a class in the embedded JVM without running it.
    /// Used to pre-load user-defined classes (data classes, etc.)
    /// before running main().
    pub fn define_class(&self, class_name: &str, class_bytes: &[u8]) -> Result<()> {
        let env = Self::jvm()
            .attach_current_thread()
            .map_err(|e| anyhow!("JNI attach failed: {e}"))?;
        let mut env = env;
        Self::define_class_in_env(&mut env, class_name, class_bytes)
    }

    fn define_class_in_env(
        env: &mut jni::JNIEnv,
        class_name: &str,
        class_bytes: &[u8],
    ) -> Result<()> {
        let loader_class = env
            .find_class("java/lang/ClassLoader")
            .map_err(|e| anyhow!("FindClass ClassLoader: {e}"))?;
        let loader = env
            .call_static_method(
                loader_class,
                "getSystemClassLoader",
                "()Ljava/lang/ClassLoader;",
                &[],
            )
            .map_err(|e| anyhow!("getSystemClassLoader: {e}"))?
            .l()
            .map_err(|e| anyhow!("getSystemClassLoader result: {e}"))?;
        let jni_name = class_name.replace('.', "/");
        env.define_class(&jni_name, &loader, class_bytes)
            .map_err(|e| {
                let _ = env.exception_clear();
                anyhow!("DefineClass `{jni_name}` failed: {e}")
            })?;
        Ok(())
    }

    fn define_and_run(
        &self,
        env: &mut jni::JNIEnv,
        class_name: &str,
        class_bytes: &[u8],
    ) -> Result<()> {
        let loader_class = env
            .find_class("java/lang/ClassLoader")
            .map_err(|e| anyhow!("FindClass ClassLoader: {e}"))?;
        let loader = env
            .call_static_method(
                loader_class,
                "getSystemClassLoader",
                "()Ljava/lang/ClassLoader;",
                &[],
            )
            .map_err(|e| anyhow!("getSystemClassLoader: {e}"))?
            .l()
            .map_err(|e| anyhow!("getSystemClassLoader result: {e}"))?;

        let jni_name = class_name.replace('.', "/");
        let defined = env
            .define_class(&jni_name, &loader, class_bytes)
            .map_err(|e| {
                let _ = env.exception_clear();
                anyhow!("DefineClass `{jni_name}` failed: {e}")
            })?;

        let string_class = env
            .find_class("java/lang/String")
            .map_err(|e| anyhow!("FindClass String: {e}"))?;
        let empty_args = env
            .new_object_array(0, string_class, JObject::null())
            .map_err(|e| anyhow!("new String[0]: {e}"))?;

        unsafe {
            let main_id = env
                .get_static_method_id(&defined, "main", "([Ljava/lang/String;)V")
                .map_err(|e| anyhow!("GetStaticMethodID main: {e}"))?;
            let result = env.call_static_method_unchecked(
                &defined,
                main_id,
                jni::signature::ReturnType::Primitive(jni::signature::Primitive::Void),
                &[JValue::Object(&JObject::from_raw(empty_args.into_raw())).as_jni()],
            );
            if result.is_err() {
                // Extract the Java exception details before clearing it.
                let detail = Self::extract_exception_detail(env);
                env.exception_clear().ok();
                return Err(anyhow!("main() call failed:\n{detail}"));
            }
        }
        Ok(())
    }

    /// Extract exception class name, message, and stack trace from a
    /// pending Java exception. Returns a formatted string for diagnostics.
    fn extract_exception_detail(env: &mut jni::JNIEnv<'_>) -> String {
        // Check if there's an exception pending.
        let ex = match env.exception_occurred() {
            Ok(ex) if !ex.is_null() => ex,
            _ => return "Java exception was thrown (no details available)".to_string(),
        };
        // Must clear the exception before making JNI calls to inspect it.
        env.exception_clear().ok();

        // Get the exception's toString() for a one-line summary.
        let summary = (|| -> Option<String> {
            let val = env
                .call_method(&ex, "toString", "()Ljava/lang/String;", &[])
                .ok()?;
            let obj = val.l().ok()?;
            let jstr = env.get_string((&obj).into()).ok()?;
            Some(jstr.to_string_lossy().into_owned())
        })()
        .unwrap_or_else(|| "(unknown exception)".to_string());

        // Get the stack trace via getStackTrace() → StackTraceElement[].
        let mut trace = String::new();
        if let Ok(arr_val) = env.call_method(
            &ex,
            "getStackTrace",
            "()[Ljava/lang/StackTraceElement;",
            &[],
        ) {
            if let Ok(arr_obj) = arr_val.l() {
                let arr: jni::objects::JObjectArray = arr_obj.into();
                let len = env.get_array_length(&arr).unwrap_or(0);
                for i in 0..len.min(20) {
                    // cap at 20 frames
                    if let Ok(elem) = env.get_object_array_element(&arr, i) {
                        if let Ok(s) =
                            env.call_method(&elem, "toString", "()Ljava/lang/String;", &[])
                        {
                            if let Ok(obj) = s.l() {
                                if let Ok(jstr) = env.get_string((&obj).into()) {
                                    trace.push_str("    at ");
                                    trace.push_str(&jstr.to_string_lossy());
                                    trace.push('\n');
                                }
                            }
                        }
                    }
                }
            }
        }

        // Get the cause chain.
        let mut cause_str = String::new();
        let mut current_cause = env
            .call_method(&ex, "getCause", "()Ljava/lang/Throwable;", &[])
            .ok()
            .and_then(|v| v.l().ok());
        while let Some(ref cause) = current_cause {
            if cause.is_null() {
                break;
            }
            if let Ok(cs) = env.call_method(cause, "toString", "()Ljava/lang/String;", &[]) {
                if let Ok(obj) = cs.l() {
                    if let Ok(jstr) = env.get_string((&obj).into()) {
                        cause_str.push_str("Caused by: ");
                        cause_str.push_str(&jstr.to_string_lossy());
                        cause_str.push('\n');
                    }
                }
            }
            current_cause = env
                .call_method(cause, "getCause", "()Ljava/lang/Throwable;", &[])
                .ok()
                .and_then(|v| v.l().ok());
        }

        format!("{summary}\n{trace}{cause_str}")
    }

    /// Verify the JVM is alive.
    pub fn check_alive(&self) -> Result<()> {
        let env = Self::jvm()
            .attach_current_thread()
            .map_err(|e| anyhow!("attach: {e}"))?;
        let mut env = env;
        env.find_class("java/lang/Object")
            .map_err(|e| anyhow!("FindClass Object: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_jvm() -> EmbeddedJvm {
        EmbeddedJvm::new().expect("JVM should initialize")
    }

    #[test]
    fn jvm_initializes_and_is_alive() {
        let jvm = get_jvm();
        jvm.check_alive().expect("should be alive");
    }

    #[test]
    fn find_class_string() {
        let _jvm = get_jvm();
        let env = EmbeddedJvm::jvm().attach_current_thread().expect("attach");
        let mut env = env;
        env.find_class("java/lang/String")
            .expect("should find String");
    }

    #[test]
    fn find_class_nonexistent() {
        let _jvm = get_jvm();
        let env = EmbeddedJvm::jvm().attach_current_thread().expect("attach");
        let mut env = env;
        let r = env.find_class("com/bogus/Nothing");
        // Should fail or set an exception.
        if r.is_ok() {
            assert!(env.exception_check().unwrap_or(false));
        }
        env.exception_clear().ok();
    }

    #[test]
    fn call_system_currenttimemillis() {
        let _jvm = get_jvm();
        let env = EmbeddedJvm::jvm().attach_current_thread().expect("attach");
        let mut env = env;
        let v = env
            .call_static_method("java/lang/System", "currentTimeMillis", "()J", &[])
            .expect("should work");
        let millis = v.j().expect("should be long");
        assert!(millis > 0);
    }
}
