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

        // Build classpath: include kotlin-stdlib.jar if found.
        let mut cp_parts: Vec<String> = Vec::new();
        if let Some(stdlib) = Self::find_kotlin_stdlib() {
            cp_parts.push(stdlib);
        }
        let mut builder = InitArgsBuilder::new().version(JNIVersion::V8);
        let cp_opt = if !cp_parts.is_empty() {
            let cp = cp_parts.join(if cfg!(windows) { ";" } else { ":" });
            Some(format!("-Djava.class.path={cp}"))
        } else {
            None
        };
        if let Some(ref opt) = cp_opt {
            builder = builder.option(opt);
        }
        let jvm_args = builder
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

    /// Locate kotlin-stdlib.jar by checking KOTLIN_HOME and kotlinc on PATH.
    fn find_kotlin_stdlib() -> Option<String> {
        // Check KOTLIN_HOME
        if let Ok(home) = std::env::var("KOTLIN_HOME") {
            let base = std::path::PathBuf::from(&home);
            for rel in ["lib/kotlin-stdlib.jar", "libexec/lib/kotlin-stdlib.jar"] {
                let p = base.join(rel);
                if p.exists() {
                    return Some(p.to_string_lossy().into_owned());
                }
            }
        }
        // Locate kotlinc on PATH and resolve symlinks
        if let Ok(kotlinc) = which::which("kotlinc") {
            if let Ok(resolved) = std::fs::canonicalize(&kotlinc) {
                if let Some(bin) = resolved.parent() {
                    if let Some(home) = bin.parent() {
                        for rel in ["lib/kotlin-stdlib.jar", "libexec/lib/kotlin-stdlib.jar"] {
                            let p = home.join(rel);
                            if p.exists() {
                                return Some(p.to_string_lossy().into_owned());
                            }
                        }
                    }
                }
            }
        }
        // Check CLASSPATH
        if let Ok(cp) = std::env::var("CLASSPATH") {
            let sep = if cfg!(windows) { ';' } else { ':' };
            for entry in cp.split(sep) {
                if entry.ends_with("kotlin-stdlib.jar") && std::path::Path::new(entry).exists() {
                    return Some(entry.to_string());
                }
            }
        }
        None
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
        if env.define_class(&jni_name, &loader, class_bytes).is_err() {
            let detail = Self::extract_exception_detail(env);
            return Err(anyhow!("DefineClass `{jni_name}` failed:\n{detail}"));
        }
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
        let defined = match env.define_class(&jni_name, &loader, class_bytes) {
            Ok(cls) => cls,
            Err(_) => {
                let detail = Self::extract_exception_detail(env);
                return Err(anyhow!("DefineClass `{jni_name}` failed:\n{detail}"));
            }
        };

        let string_class = env
            .find_class("java/lang/String")
            .map_err(|e| anyhow!("FindClass String: {e}"))?;
        let empty_args = env
            .new_object_array(0, string_class, JObject::null())
            .map_err(|e| anyhow!("new String[0]: {e}"))?;

        unsafe {
            let main_id = match env.get_static_method_id(&defined, "main", "([Ljava/lang/String;)V")
            {
                Ok(id) => id,
                Err(_) => {
                    let detail = Self::extract_exception_detail(env);
                    return Err(anyhow!("GetStaticMethodID main failed:\n{detail}"));
                }
            };
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

    /// Add a JAR file to the JVM classpath by appending it to the system
    /// class loader's URL list. Uses reflection to call the package-private
    /// `addURL` method on `URLClassLoader`.
    pub fn add_jar_to_classpath(&self, jar_path: &std::path::Path) -> Result<()> {
        let mut env = Self::jvm()
            .attach_current_thread()
            .map_err(|e| anyhow!("attach: {e}"))?;

        // Convert path to a file:// URL string.
        let abs = jar_path
            .canonicalize()
            .unwrap_or_else(|_| jar_path.to_path_buf());
        let url_str = format!("file://{}", abs.display());

        // Create a java.net.URL object.
        let url_class = env
            .find_class("java/net/URL")
            .map_err(|e| anyhow!("FindClass URL: {e}"))?;
        let url_str_j = env
            .new_string(&url_str)
            .map_err(|e| anyhow!("NewString: {e}"))?;
        let url_obj = env
            .new_object(
                &url_class,
                "(Ljava/lang/String;)V",
                &[jni::objects::JValue::Object(&url_str_j.into())],
            )
            .map_err(|e| anyhow!("new URL({url_str}): {e}"))?;

        // Get the system class loader (should be a URLClassLoader).
        let cl_class = env
            .find_class("java/lang/ClassLoader")
            .map_err(|e| anyhow!("FindClass ClassLoader: {e}"))?;
        let sys_cl = env
            .call_static_method(
                &cl_class,
                "getSystemClassLoader",
                "()Ljava/lang/ClassLoader;",
                &[],
            )
            .map_err(|e| anyhow!("getSystemClassLoader: {e}"))?
            .l()
            .map_err(|e| anyhow!("getSystemClassLoader result: {e}"))?;

        // Use reflection to call addURL on the URLClassLoader.
        // Only safe when the system class loader is actually a
        // URLClassLoader — on Java 9+ it is an `AppClassLoader` that
        // does NOT extend URLClassLoader, and invoking the method via
        // a URLClassLoader method id is JNI undefined behavior that
        // crashes the JVM on modern JDKs.
        let ucl_class = env
            .find_class("java/net/URLClassLoader")
            .map_err(|e| anyhow!("FindClass URLClassLoader: {e}"))?;
        let is_url_cl = env
            .is_instance_of(&sys_cl, &ucl_class)
            .map_err(|e| anyhow!("IsInstanceOf URLClassLoader: {e}"))?;
        if !is_url_cl {
            return Err(anyhow!(
                "cannot add jar to system classpath at runtime: system class loader \
                 is not a URLClassLoader (Java 9+). Set the classpath before JVM \
                 startup instead. JAR: {url_str}"
            ));
        }
        let add_url_method = env
            .get_method_id(&ucl_class, "addURL", "(Ljava/net/URL;)V")
            .map_err(|e| anyhow!("GetMethodID URLClassLoader.addURL: {e}"))?;
        unsafe {
            let _ = env.call_method_unchecked(
                &sys_cl,
                add_url_method,
                jni::signature::ReturnType::Primitive(jni::signature::Primitive::Void),
                &[jni::objects::JValue::Object(&url_obj).as_jni()],
            );
        }
        env.exception_clear().ok();

        Ok(())
    }

    /// List public method and field names for a class via JVM reflection.
    ///
    /// Uses `Class.forName(name)` to load the class, then `getMethods()`
    /// and `getFields()` to enumerate members. Method overloads are
    /// deduplicated (only the name is returned). Returns `(name, kind)`
    /// pairs where kind is `"method"` or `"field"`.
    pub fn list_members(&self, java_class_name: &str) -> Result<Vec<(String, &'static str)>> {
        let mut env = Self::jvm()
            .attach_current_thread()
            .map_err(|e| anyhow!("attach: {e}"))?;

        let name_j = env
            .new_string(java_class_name)
            .map_err(|e| anyhow!("NewString: {e}"))?;
        let cls = match env.call_static_method(
            "java/lang/Class",
            "forName",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&name_j.into())],
        ) {
            Ok(v) => v.l().map_err(|e| anyhow!("forName result: {e}"))?,
            Err(_) => {
                env.exception_clear().ok();
                return Err(anyhow!("Class.forName({java_class_name}) not found"));
            }
        };

        let mut members: Vec<(String, &'static str)> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // Public methods.
        if let Ok(arr_val) =
            env.call_method(&cls, "getMethods", "()[Ljava/lang/reflect/Method;", &[])
        {
            if let Ok(arr_obj) = arr_val.l() {
                let arr: jni::objects::JObjectArray = arr_obj.into();
                let len = env.get_array_length(&arr).unwrap_or(0);
                for i in 0..len {
                    if let Ok(elem) = env.get_object_array_element(&arr, i) {
                        if let Ok(nv) =
                            env.call_method(&elem, "getName", "()Ljava/lang/String;", &[])
                        {
                            if let Ok(no) = nv.l() {
                                if let Ok(js) = env.get_string((&no).into()) {
                                    let n = js.to_string_lossy().into_owned();
                                    if seen.insert(n.clone()) {
                                        members.push((n, "method"));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        env.exception_clear().ok();

        // Public fields.
        if let Ok(arr_val) = env.call_method(&cls, "getFields", "()[Ljava/lang/reflect/Field;", &[])
        {
            if let Ok(arr_obj) = arr_val.l() {
                let arr: jni::objects::JObjectArray = arr_obj.into();
                let len = env.get_array_length(&arr).unwrap_or(0);
                for i in 0..len {
                    if let Ok(elem) = env.get_object_array_element(&arr, i) {
                        if let Ok(nv) =
                            env.call_method(&elem, "getName", "()Ljava/lang/String;", &[])
                        {
                            if let Ok(no) = nv.l() {
                                if let Ok(js) = env.get_string((&no).into()) {
                                    let n = js.to_string_lossy().into_owned();
                                    if seen.insert(n.clone()) {
                                        members.push((n, "field"));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        env.exception_clear().ok();

        members.sort();
        Ok(members)
    }

    /// Scan a JAR file for class names using `java.util.jar.JarFile`.
    ///
    /// Returns fully-qualified class names (e.g. `com.example.Foo`).
    /// Inner classes (containing `$`) and metadata entries are excluded.
    pub fn scan_jar_classes(&self, jar_path: &std::path::Path) -> Result<Vec<String>> {
        let mut env = Self::jvm()
            .attach_current_thread()
            .map_err(|e| anyhow!("attach: {e}"))?;

        let path_str = jar_path.to_string_lossy();
        let path_j = env
            .new_string(&*path_str)
            .map_err(|e| anyhow!("NewString: {e}"))?;

        let jar_class = env
            .find_class("java/util/jar/JarFile")
            .map_err(|e| anyhow!("FindClass JarFile: {e}"))?;
        let jar_obj = match env.new_object(
            jar_class,
            "(Ljava/lang/String;)V",
            &[JValue::Object(&path_j.into())],
        ) {
            Ok(j) => j,
            Err(_) => {
                env.exception_clear().ok();
                return Err(anyhow!("cannot open JAR: {path_str}"));
            }
        };

        let entries = env
            .call_method(&jar_obj, "entries", "()Ljava/util/Enumeration;", &[])
            .map_err(|e| anyhow!("entries(): {e}"))?
            .l()
            .map_err(|e| anyhow!("entries result: {e}"))?;

        let mut classes = Vec::new();

        loop {
            let has = env
                .call_method(&entries, "hasMoreElements", "()Z", &[])
                .and_then(|v| v.z())
                .unwrap_or(false);
            if !has {
                break;
            }
            let entry = match env.call_method(&entries, "nextElement", "()Ljava/lang/Object;", &[])
            {
                Ok(v) => match v.l() {
                    Ok(o) => o,
                    _ => continue,
                },
                _ => break,
            };
            let name = match env.call_method(&entry, "getName", "()Ljava/lang/String;", &[]) {
                Ok(nv) => match nv.l() {
                    Ok(no) => match env.get_string((&no).into()) {
                        Ok(js) => js.to_string_lossy().into_owned(),
                        _ => continue,
                    },
                    _ => continue,
                },
                _ => continue,
            };
            if name.ends_with(".class") && !name.contains('$') {
                if let Some(cn) = name.strip_suffix(".class") {
                    let dotted = cn.replace('/', ".");
                    if !dotted.ends_with(".module-info") && !dotted.ends_with(".package-info") {
                        classes.push(dotted);
                    }
                }
            }
        }

        let _ = env.call_method(&jar_obj, "close", "()V", &[]);
        env.exception_clear().ok();
        Ok(classes)
    }

    /// Discover all classes available to the JVM at startup.
    ///
    /// - **Java 9+**: scans every `.jmod` file under `$JAVA_HOME/jmods/`.
    ///   `.jmod` files are ZIP archives with a 4-byte `JM` header; we
    ///   skip that header and read entries via `ZipInputStream`.
    /// - **Java 8**: scans `$JAVA_HOME/lib/rt.jar` with [`scan_jar_classes`].
    ///
    /// Returns fully-qualified class names (e.g. `java.lang.String`).
    /// Inner classes (`$`) and metadata entries are excluded.
    pub fn scan_system_classes(&self) -> Result<Vec<String>> {
        let java_home = locate::resolve_java_home()?;
        let real_home = std::fs::canonicalize(&java_home).unwrap_or(java_home.clone());

        // Candidate roots — the same two that `find_libjvm` probes.
        // Homebrew on macOS nests the real JDK under libexec/.
        let roots = [
            real_home.clone(),
            real_home.join("libexec/openjdk.jdk/Contents/Home"),
            java_home.clone(),
            java_home.join("libexec/openjdk.jdk/Contents/Home"),
        ];

        // Java 9+: scan jmods/ directory.
        for root in &roots {
            let jmods = root.join("jmods");
            if jmods.is_dir() {
                let mut all = Vec::new();
                if let Ok(entries) = std::fs::read_dir(&jmods) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.extension().and_then(|e| e.to_str()) == Some("jmod") {
                            match self.scan_jmod_file(&p) {
                                Ok(cs) => all.extend(cs),
                                Err(_) => continue,
                            }
                        }
                    }
                }
                if !all.is_empty() {
                    return Ok(all);
                }
            }
        }

        // Java 8 fallback: rt.jar.
        for root in &roots {
            for rel in ["lib/rt.jar", "jre/lib/rt.jar"] {
                let rt = root.join(rel);
                if rt.exists() {
                    return self.scan_jar_classes(&rt);
                }
            }
        }

        Ok(Vec::new())
    }

    /// Scan a `.jmod` file for class names.
    ///
    /// `.jmod` files are ZIP archives preceded by a 4-byte magic
    /// header (`JM\x01\x00`). We open a `FileInputStream`, skip the
    /// header, and feed the remainder into a `ZipInputStream`.
    /// Class entries live under the `classes/` prefix.
    fn scan_jmod_file(&self, path: &std::path::Path) -> Result<Vec<String>> {
        let mut env = Self::jvm()
            .attach_current_thread()
            .map_err(|e| anyhow!("attach: {e}"))?;

        let path_str = path.to_string_lossy();
        let path_j = env
            .new_string(&*path_str)
            .map_err(|e| anyhow!("NewString: {e}"))?;

        // new FileInputStream(path)
        let fis_class = env
            .find_class("java/io/FileInputStream")
            .map_err(|e| anyhow!("FindClass FileInputStream: {e}"))?;
        let fis = match env.new_object(
            fis_class,
            "(Ljava/lang/String;)V",
            &[JValue::Object(&path_j.into())],
        ) {
            Ok(f) => f,
            Err(_) => {
                env.exception_clear().ok();
                return Err(anyhow!("cannot open: {path_str}"));
            }
        };

        // fis.skip(4) — skip the JM\x01\x00 header
        let _ = env.call_method(&fis, "skip", "(J)J", &[JValue::Long(4)]);
        env.exception_clear().ok();

        // new ZipInputStream(fis)
        let zis_class = env
            .find_class("java/util/zip/ZipInputStream")
            .map_err(|e| anyhow!("FindClass ZipInputStream: {e}"))?;
        let zis = match env.new_object(
            zis_class,
            "(Ljava/io/InputStream;)V",
            &[JValue::Object(&fis)],
        ) {
            Ok(z) => z,
            Err(_) => {
                env.exception_clear().ok();
                let _ = env.call_method(&fis, "close", "()V", &[]);
                return Err(anyhow!("ZipInputStream failed: {path_str}"));
            }
        };

        let mut classes = Vec::new();

        loop {
            let entry_val =
                match env.call_method(&zis, "getNextEntry", "()Ljava/util/zip/ZipEntry;", &[]) {
                    Ok(v) => v,
                    Err(_) => {
                        env.exception_clear().ok();
                        break;
                    }
                };
            let entry = match entry_val.l() {
                Ok(o) if !o.is_null() => o,
                _ => break,
            };

            let name = (|| -> Option<String> {
                let nv = env
                    .call_method(&entry, "getName", "()Ljava/lang/String;", &[])
                    .ok()?;
                let no = nv.l().ok()?;
                let s = env
                    .get_string((&no).into())
                    .ok()?
                    .to_string_lossy()
                    .into_owned();
                let _ = no; // ensure no is not used after get_string
                Some(s)
            })();

            env.delete_local_ref(entry).ok();

            if let Some(name) = name {
                if name.starts_with("classes/") && name.ends_with(".class") && !name.contains('$') {
                    if let Some(cn) = name
                        .strip_prefix("classes/")
                        .and_then(|s| s.strip_suffix(".class"))
                    {
                        let dotted = cn.replace('/', ".");
                        if !dotted.ends_with("module-info") && !dotted.ends_with("package-info") {
                            classes.push(dotted);
                        }
                    }
                }
            }
        }

        let _ = env.call_method(&zis, "close", "()V", &[]);
        env.exception_clear().ok();
        Ok(classes)
    }

    /// Verify the JVM is alive.
    /// Return the JVM version string (e.g. `"25.0.1"`).
    pub fn java_version(&self) -> Result<String> {
        let mut env = Self::jvm()
            .attach_current_thread()
            .map_err(|e| anyhow!("attach: {e}"))?;
        let key = env
            .new_string("java.version")
            .map_err(|e| anyhow!("NewString: {e}"))?;
        let val = env
            .call_static_method(
                "java/lang/System",
                "getProperty",
                "(Ljava/lang/String;)Ljava/lang/String;",
                &[JValue::Object(&key.into())],
            )
            .map_err(|e| anyhow!("getProperty: {e}"))?
            .l()
            .map_err(|e| anyhow!("getProperty result: {e}"))?;
        if val.is_null() {
            return Ok("unknown".to_string());
        }
        let s = env
            .get_string((&val).into())
            .map_err(|e| anyhow!("getString: {e}"))?
            .to_string_lossy()
            .into_owned();
        Ok(s)
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

    #[test]
    fn scan_system_classes_finds_jdk() {
        let jvm = get_jvm();
        let classes = jvm.scan_system_classes().expect("should scan JDK");
        // Any modern JDK should have thousands of classes.
        assert!(
            classes.len() > 1000,
            "expected >1000 system classes, got {}",
            classes.len()
        );
        // Smoke-check a few well-known ones.
        assert!(classes.contains(&"java.lang.String".to_string()));
        assert!(classes.contains(&"java.util.HashMap".to_string()));
        assert!(classes.contains(&"java.io.File".to_string()));
    }
}
