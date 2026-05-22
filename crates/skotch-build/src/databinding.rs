//! Stub data-binding class generator for Android layouts.
//!
//! Android view-binding normally runs as a Gradle/aapt2 step that parses
//! `res/layout/<file>.xml` and emits a Java class implementing
//! `androidx.viewbinding.ViewBinding`. Skotch doesn't ship aapt2's
//! binding generator, so calls like `AndroidViewBinding(ContentMainBinding::inflate)`
//! fail at runtime with `NoClassDefFoundError`/`ClassCastException`.
//!
//! This module synthesises minimal Kotlin source files for each layout —
//! enough to satisfy the type system and the `factory.invoke(...)` call
//! inside `AndroidViewBinding`. The generated `inflate(...)` doesn't
//! actually parse the XML; it constructs a plain `FrameLayout` plus a
//! placeholder child for each `android:id`. NavHostFragment-backed
//! screens won't render, but the surrounding Compose UI is no longer
//! blocked.

use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A child view declared in a layout XML with an `android:id`.
#[derive(Clone, Debug)]
struct ChildView {
    /// Field name in CamelCase (e.g. `navHostFragment` from `nav_host_fragment`).
    field_name: String,
    /// Tag name (e.g. `androidx.fragment.app.FragmentContainerView`). Used to
    /// pick the Kotlin/Java type for the field — fully-qualified or simple.
    tag: String,
}

#[derive(Clone, Debug)]
struct LayoutBinding {
    /// Source layout file stem (e.g. `content_main`).
    layout_stem: String,
    /// Generated binding class name (e.g. `ContentMainBinding`).
    class_name: String,
    /// Root element tag (e.g. `FrameLayout`).
    root_tag: String,
    /// Per-id child views.
    children: Vec<ChildView>,
}

/// Scan `res_dir/layout/*.xml`, generate stub binding sources under
/// `out_dir/<pkg_path>/databinding/`, and return the list of generated
/// file paths so the build pipeline can add them to its source set.
pub fn generate_binding_stubs(
    res_dir: &Path,
    app_package: &str,
    out_dir: &Path,
) -> Result<Vec<PathBuf>> {
    let layout_dir = res_dir.join("layout");
    if !layout_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut bindings: Vec<LayoutBinding> = Vec::new();
    for entry in walkdir::WalkDir::new(&layout_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.path().extension().and_then(|s| s.to_str()) != Some("xml") {
            continue;
        }
        let stem = match entry.path().file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let Ok(xml) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Some(parsed) = parse_layout(&xml, &stem) else {
            continue;
        };
        bindings.push(parsed);
    }
    if bindings.is_empty() {
        return Ok(Vec::new());
    }

    let pkg_path = format!("{}/databinding", app_package.replace('.', "/"));
    let pkg_dir = out_dir.join(&pkg_path);
    std::fs::create_dir_all(&pkg_dir)?;

    let mut written: Vec<PathBuf> = Vec::new();
    for b in &bindings {
        let src = render_binding_kotlin(b, app_package);
        let file = pkg_dir.join(format!("{}.kt", b.class_name));
        std::fs::write(&file, src)?;
        written.push(file);
    }
    Ok(written)
}

/// Convert `content_main` → `ContentMainBinding`.
fn binding_class_name(layout_stem: &str) -> String {
    let mut out = String::new();
    let mut upper_next = true;
    for c in layout_stem.chars() {
        if c == '_' {
            upper_next = true;
            continue;
        }
        if upper_next {
            out.push(c.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out.push_str("Binding");
    out
}

/// Convert `nav_host_fragment` → `navHostFragment`.
fn camel_case_id(id: &str) -> String {
    let mut out = String::new();
    let mut upper_next = false;
    for c in id.chars() {
        if c == '_' {
            upper_next = true;
            continue;
        }
        if upper_next {
            out.push(c.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Pick a Kotlin type for an XML element tag. The full FQ type is best
/// (matches gradle), but the stub only needs the type name to compile —
/// `android.view.View` is always-safe.
fn jvm_type_for_tag(tag: &str) -> &'static str {
    // For the stub, default everything to View — the binding's actual
    // type signature doesn't matter at runtime because skotch never
    // routes through the real ViewBinding factory.
    let _ = tag;
    "android.view.View"
}

/// Pick the root layout type. Default to FrameLayout for safety.
fn root_view_type(tag: &str) -> &'static str {
    match tag {
        "LinearLayout" => "android.widget.LinearLayout",
        "RelativeLayout" => "android.widget.RelativeLayout",
        "ScrollView" => "android.widget.ScrollView",
        "androidx.constraintlayout.widget.ConstraintLayout" => {
            "androidx.constraintlayout.widget.ConstraintLayout"
        }
        "androidx.coordinatorlayout.widget.CoordinatorLayout" => {
            "androidx.coordinatorlayout.widget.CoordinatorLayout"
        }
        _ => "android.widget.FrameLayout",
    }
}

/// Render a minimal Kotlin binding source.
///
/// Generates roughly the same body as AGP's ViewBinding processor: the
/// 3-arg `inflate` calls the framework `LayoutInflater.inflate(int,
/// ViewGroup, boolean)` with the layout's `R.layout.<stem>` id, then
/// runs `findViewById(R.id.<id>)` for each declared id. We keep the
/// child fields typed as `android.view.View` because deriving the real
/// child types from an XML tag (e.g. `androidx.fragment.app.FragmentContainerView`)
/// needs full type-erasure-safe lookup and isn't worth it for the stub.
fn render_binding_kotlin(b: &LayoutBinding, app_package: &str) -> String {
    let root_ty_fq = root_view_type(&b.root_tag);
    let root_ty_simple = root_ty_fq.rsplit('.').next().unwrap_or(root_ty_fq);
    let mut src = String::new();
    use std::fmt::Write;
    let _ = writeln!(src, "package {}.databinding", app_package);
    src.push('\n');
    let _ = writeln!(src, "import {}.R", app_package);
    src.push_str("import android.view.LayoutInflater\n");
    src.push_str("import android.view.View\n");
    src.push_str("import android.view.ViewGroup\n");
    // Skotch's resolver currently only resolves *imported* simple names
    // to their JVM descriptors; an FQ name like `android.widget.FrameLayout`
    // used in-place falls back to `Object` and breaks the `<init>`
    // descriptor at runtime. Import the root view type and use its
    // simple form everywhere below.
    let _ = writeln!(src, "import {}", root_ty_fq);
    src.push_str("import androidx.viewbinding.ViewBinding\n");
    src.push('\n');
    let _ = writeln!(src, "class {} (", b.class_name);
    let _ = writeln!(src, "    private val rootView: {},", root_ty_simple);
    for c in &b.children {
        let _ = writeln!(
            src,
            "    val {}: {},",
            c.field_name,
            jvm_type_for_tag(&c.tag)
        );
    }
    src.push_str(") : ViewBinding {\n");
    let _ = writeln!(src, "    override fun getRoot(): View = rootView");
    src.push_str("    companion object {\n");
    let _ = writeln!(
        src,
        "        @JvmStatic fun inflate(inflater: LayoutInflater): {} = inflate(inflater, null, false)",
        b.class_name
    );
    let _ = writeln!(
        src,
        "        @JvmStatic fun inflate(inflater: LayoutInflater, parent: ViewGroup?, attachToParent: Boolean): {} {{",
        b.class_name
    );
    let _ = writeln!(
        src,
        "            val root = inflater.inflate(R.layout.{}, parent, attachToParent) as {}",
        b.layout_stem, root_ty_simple
    );
    for c in &b.children {
        let r_id = snake_case_id(&c.field_name);
        let _ = writeln!(
            src,
            "            val {} = root.findViewById<View>(R.id.{})",
            c.field_name, r_id
        );
    }
    let _ = write!(src, "            return {}(root", b.class_name);
    for c in &b.children {
        let _ = write!(src, ", {}", c.field_name);
    }
    src.push_str(")\n");
    src.push_str("        }\n");
    src.push_str("    }\n");
    src.push_str("}\n");
    src
}

/// Reverse `camelCase` → `snake_case` so R-class lookups use the
/// original layout-XML id name.
fn snake_case_id(camel: &str) -> String {
    let mut out = String::new();
    for (i, c) in camel.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Parse the layout XML to find the root element tag and any
/// `android:id="@+id/<name>"` declarations. Doesn't try to be a real
/// XML parser — JetChat's layouts are simple enough for this.
fn parse_layout(xml: &str, layout_stem: &str) -> Option<LayoutBinding> {
    let _ = layout_stem; // retained for future per-layout namespacing
                         // Find the first non-comment, non-decl element start tag.
    let mut chars = xml.char_indices().peekable();
    let mut root_tag: Option<String> = None;
    while let Some((_, c)) = chars.next() {
        if c != '<' {
            continue;
        }
        // Skip XML decl `<?` and comments `<!`.
        if let Some(&(_, next)) = chars.peek() {
            if next == '?' || next == '!' {
                continue;
            }
        }
        // Read tag name up to whitespace / '>'.
        let mut name = String::new();
        while let Some(&(_, n)) = chars.peek() {
            if n.is_whitespace() || n == '>' || n == '/' {
                break;
            }
            name.push(n);
            chars.next();
        }
        if !name.is_empty() {
            root_tag = Some(name);
            break;
        }
    }
    let root_tag = root_tag?;

    // Collect (id, tag) for every element that declares android:id.
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    let mut children: Vec<ChildView> = Vec::new();
    let mut i = 0usize;
    let bytes = xml.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Bare-minimum: scan the next element until '>'. Inside, look
            // for `android:id="@+id/<name>"`.
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'>' {
                j += 1;
            }
            let elem = &xml[start..j.min(bytes.len())];
            // Element name = leading non-whitespace token.
            let tag: String = elem
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '/' && *c != '>')
                .collect();
            if !tag.is_empty() && !tag.starts_with('?') && !tag.starts_with('!') {
                if let Some(id) = extract_id_attr(elem) {
                    if seen_ids.insert(id.clone()) {
                        children.push(ChildView {
                            field_name: camel_case_id(&id),
                            tag: tag.clone(),
                        });
                    }
                }
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }

    Some(LayoutBinding {
        layout_stem: layout_stem.to_string(),
        class_name: binding_class_name(layout_stem),
        root_tag,
        children,
    })
}

/// Pull `android:id="@+id/<name>"` (or `@id/<name>`) out of an element's
/// attribute soup. Returns the raw id string (e.g. `nav_host_fragment`).
fn extract_id_attr(elem: &str) -> Option<String> {
    let key = "android:id";
    let start = elem.find(key)?;
    let rest = &elem[start + key.len()..];
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.strip_prefix('"').or_else(|| rest.strip_prefix('\''))?;
    let end = rest.find(['"', '\''])?;
    let value = &rest[..end];
    let id = value
        .strip_prefix("@+id/")
        .or_else(|| value.strip_prefix("@id/"))?;
    Some(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_case_basic() {
        assert_eq!(camel_case_id("nav_host_fragment"), "navHostFragment");
        assert_eq!(camel_case_id("toolbar"), "toolbar");
        assert_eq!(camel_case_id("a_b"), "aB");
    }

    #[test]
    fn binding_class_basic() {
        assert_eq!(binding_class_name("content_main"), "ContentMainBinding");
        assert_eq!(
            binding_class_name("fragment_profile"),
            "FragmentProfileBinding"
        );
    }

    #[test]
    fn parse_layout_content_main() {
        let xml = r#"<?xml version="1.0"?>
<FrameLayout xmlns:android="http://schemas.android.com/apk/res/android">
  <androidx.fragment.app.FragmentContainerView
      android:id="@+id/nav_host_fragment" />
</FrameLayout>"#;
        let b = parse_layout(xml, "content_main").unwrap();
        assert_eq!(b.class_name, "ContentMainBinding");
        assert_eq!(b.root_tag, "FrameLayout");
        assert_eq!(b.children.len(), 1);
        assert_eq!(b.children[0].field_name, "navHostFragment");
    }
}
