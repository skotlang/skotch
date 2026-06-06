//! Annotation lowering — converts `skotch_syntax::Annotation` and bare
//! annotation-name lists (e.g. those carried on cross-file
//! `ExternalMethod` / `ExternalClassDecl`) into MIR-level
//! `MirAnnotation` entries.
//!
//! Two retention flavors are supported:
//!  * **Source**: dropped entirely (Suppress, OptIn, …) — these don't
//!    appear in bytecode.
//!  * **Binary** vs **Runtime**: chosen by descriptor; Composable et al.
//!    use Binary so the `RuntimeInvisibleAnnotations` attribute is
//!    emitted instead of `RuntimeVisibleAnnotations`.

use rustc_hash::FxHashMap;
use skotch_intern::Interner;

pub(crate) fn is_source_retention_annotation(name: &str) -> bool {
    matches!(
        name,
        "Suppress"
            | "SinceKotlin"
            | "OptIn"
            | "RequiresOptIn"
            | "SubclassOptInRequired"
            | "BuilderInference"
            | "Experimental"
            | "kotlin/Suppress"
            | "kotlin/SinceKotlin"
            | "kotlin/OptIn"
            | "kotlin/RequiresOptIn"
            | "kotlin/BuilderInference"
            | "kotlin/Experimental"
    )
}

pub(crate) fn lower_annotations(
    annotations: &[skotch_syntax::Annotation],
    interner: &Interner,
    import_map: Option<&FxHashMap<String, String>>,
) -> Vec<skotch_mir::MirAnnotation> {
    annotations
        .iter()
        .filter(|a| !is_source_retention_annotation(interner.resolve(a.name)))
        .map(|a| {
            let name = interner.resolve(a.name);
            // First try the well-known annotation registry (JvmStatic, etc.).
            let mut descriptor = skotch_stdlib_registry::annotation_descriptor(name);
            // If the registry returned a simple-name descriptor (no package),
            // try to resolve via the file's import_map. E.g. `import
            // org.junit.jupiter.api.Test` maps "Test" → "org/junit/jupiter/api/Test",
            // giving descriptor "Lorg/junit/jupiter/api/Test;".
            if !descriptor.contains('/') {
                if let Some(map) = import_map {
                    if let Some(jvm_path) = map.get(name) {
                        descriptor = format!("L{jvm_path};");
                    }
                }
            }
            let args: Vec<skotch_mir::MirAnnotationArg> = a
                .args
                .iter()
                .enumerate()
                .map(|(i, arg)| {
                    let arg_name = format!("value{i}");
                    let value = match arg {
                        skotch_syntax::AnnotationArg::StringLit(s) => {
                            skotch_mir::MirAnnotationValue::String(s.clone())
                        }
                        skotch_syntax::AnnotationArg::IntLit(v) => {
                            skotch_mir::MirAnnotationValue::Int(*v as i32)
                        }
                        skotch_syntax::AnnotationArg::BoolLit(v) => {
                            skotch_mir::MirAnnotationValue::Bool(*v)
                        }
                        skotch_syntax::AnnotationArg::Ident(sym) => {
                            skotch_mir::MirAnnotationValue::String(
                                interner.resolve(*sym).to_string(),
                            )
                        }
                        skotch_syntax::AnnotationArg::QualifiedName(parts) => {
                            let joined: Vec<&str> =
                                parts.iter().map(|s| interner.resolve(*s)).collect();
                            skotch_mir::MirAnnotationValue::String(joined.join("."))
                        }
                        skotch_syntax::AnnotationArg::Array(items) => {
                            let arr: Vec<skotch_mir::MirAnnotationValue> = items
                                .iter()
                                .map(|item| match item {
                                    skotch_syntax::AnnotationArg::StringLit(s) => {
                                        skotch_mir::MirAnnotationValue::String(s.clone())
                                    }
                                    _ => skotch_mir::MirAnnotationValue::String(String::new()),
                                })
                                .collect();
                            skotch_mir::MirAnnotationValue::Array(arr)
                        }
                    };
                    skotch_mir::MirAnnotationArg {
                        name: if a.args.len() == 1 {
                            "value".to_string()
                        } else {
                            arg_name
                        },
                        value,
                    }
                })
                .collect();
            // Default to Runtime, but flip to Binary for annotations
            // declared with @Retention(AnnotationRetention.BINARY) by
            // common stdlib / Compose / Kotlin types. kotlinc emits
            // these via RuntimeInvisibleAnnotations; matching the
            // retention keeps the attribute placement byte-identical.
            let retention = if is_binary_retention_annotation(&descriptor) {
                skotch_mir::AnnotationRetention::Binary
            } else {
                skotch_mir::AnnotationRetention::Runtime
            };
            skotch_mir::MirAnnotation {
                descriptor,
                args,
                retention,
            }
        })
        .collect()
}

pub(crate) fn is_binary_retention_annotation(descriptor: &str) -> bool {
    // Common @Composable / Kotlin compiler-plugin annotations whose
    // canonical declaration uses `@Retention(AnnotationRetention.BINARY)`.
    matches!(
        descriptor,
        "Landroidx/compose/runtime/Composable;"
            | "Landroidx/compose/runtime/ReadOnlyComposable;"
            | "Landroidx/compose/runtime/Stable;"
            | "Landroidx/compose/runtime/Immutable;"
            | "Landroidx/compose/runtime/NonRestartableComposable;"
    )
}

/// Convert a list of annotation simple names (e.g. those carried on a
/// cross-file `ExternalMethod` / `ExternalClassDecl`) into
/// `MirAnnotation` entries. Skips source-retention annotations and
/// picks the right Binary/Runtime retention via the stdlib registry.
/// Centralizes the propagation pattern that was inlined at every
/// cross-file stub builder site.
pub(crate) fn annotations_from_names<I, S>(names: I) -> Vec<skotch_mir::MirAnnotation>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    names
        .into_iter()
        .filter(|n| !is_source_retention_annotation(n.as_ref()))
        .map(|n| {
            let descriptor = skotch_stdlib_registry::annotation_descriptor(n.as_ref());
            let retention = if is_binary_retention_annotation(&descriptor) {
                skotch_mir::AnnotationRetention::Binary
            } else {
                skotch_mir::AnnotationRetention::Runtime
            };
            skotch_mir::MirAnnotation {
                descriptor,
                args: Vec::new(),
                retention,
            }
        })
        .collect()
}
