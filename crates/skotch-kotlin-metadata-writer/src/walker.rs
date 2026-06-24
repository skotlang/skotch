//! MIR → `ProtoBuf.Class` / `ProtoBuf.Package` walker.
//!
//! Phase B of the `@kotlin.Metadata` writer pipeline. Phase A laid down
//! the prost-generated message types plus the `BitEncoding` codec that
//! packs raw protobuf bytes into the `d1` `String[]`; this module is the
//! layer above that takes a [`skotch_mir::MirClass`] / [`skotch_mir::MirModule`]
//! and emits a fully populated `ProtoBuf.Class` (for `k=1` class metadata)
//! or `ProtoBuf.Package` (for `k=2` file-facade metadata).
//!
//! ## Wire format
//!
//! The raw `d1` payload is a length-delimited
//! `JvmProtoBuf.StringTableTypes` message followed by the
//! `ProtoBuf.Class` or `ProtoBuf.Package` body. That's the
//! `JvmProtoBufUtil.readPackageDataFrom` shape — the very same format
//! [`skotch_classinfo::kotlin_metadata::parse_metadata`] consumes on
//! the decoder side. We deliberately mirror its decode logic to know
//! what must be encoded.
//!
//! ## String table strategy
//!
//! kotlinc's `JvmNameResolverBase.getString(i)` does an UNCHECKED
//! `records.get(i)`. If our payload references a name index that is
//! out of bounds for the records list, kotlinc 2.4 throws
//! `IndexOutOfBoundsException` deep in metadata deserialization. Our
//! own reader is more permissive (it falls back to the raw `d2` array
//! when a record is missing), but the strict kotlinc resolver requires
//! a record for EVERY referenced index.
//!
//! Strategy: emit one "default" record per `d2` entry. A default record
//! has neither `string` nor `predefined_index` set, which causes the
//! resolver to fall through to `strings[i]` (the `d2` array entry).
//! To stay compact we use the `range` field — a single record with
//! `range = N` expands (via `JvmNameResolverKt.toExpandedRecordsList`)
//! into N identical empty records, one per `d2` index. Net wire cost
//! is a few bytes for a single record header regardless of `d2` size,
//! while satisfying kotlinc 2.4's strict bounds check.
//!
//! ## Coverage
//!
//! Populated on each `Class`:
//!   * `fq_name` (index of the class's slash-separated JVM-style name)
//!   * `flags` — a placeholder kotlinc 2.x-compatible value
//!   * `constructor[].value_parameter[].{name, type}` (primary + secondary)
//!   * `function[].{name, value_parameter[], return_type}`
//!   * `property[].{name, return_type}` for instance + static fields
//!   * `companion_object_name` and `nested_class_name` when applicable
//!
//! Populated on the file-facade `Package`:
//!   * `function[].{name, value_parameter[], return_type}` for every
//!     top-level function in the [`MirModule`]
//!   * `property[].{name, return_type}` for top-level vals/consts
//!
//! Generic-arg / type-parameter / annotation / contract emission is
//! deliberately out of scope for Phase B; the descriptors we feed
//! through `jvm_descriptor_to_type` cover what the consumer-side
//! `100-clikt` named-arg reorder needs.

use prost::Message;
use skotch_mir::{MirClass, MirField, MirFunction, MirModule};
use skotch_types::Ty;

use crate::bit_encoding::encode_bytes;
use crate::proto::org::jetbrains::kotlin::metadata as pb;
use crate::proto::org::jetbrains::kotlin::metadata::jvm as jvm_pb;

/// Encoded `@kotlin.Metadata` payload ready for the JVM-backend emit
/// site to stamp onto a class file. `k` is the kotlinc metadata kind
/// (1 = class, 2 = file facade). `mv` is the metadata-version triple.
/// `d1` is the bit-encoded payload (`String[]`); `d2` is the raw
/// string table (`String[]`). `xs`/`pn`/`xi` are vestigial fields the
/// runtime expects but the current writer leaves at their defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    /// `@Metadata.k`: 1 = class, 2 = file facade.
    pub k: i32,
    /// `@Metadata.mv`: metadata-version triple, e.g. `[2, 4, 0]` for kotlinc 2.4.
    pub mv: Vec<i32>,
    /// `@Metadata.d1`: bit-encoded protobuf payload (string-array form).
    pub d1: Vec<String>,
    /// `@Metadata.d2`: the raw string table the payload's int32 indices
    /// reference (insertion-order stable).
    pub d2: Vec<String>,
    /// `@Metadata.xs`: extra string (kotlinc uses this for multi-file
    /// facades only). Default empty.
    pub xs: String,
    /// `@Metadata.pn`: package name. Default empty (the JVM name in
    /// `fq_name` already encodes the package).
    pub pn: String,
    /// `@Metadata.xi`: extra integer (bit flags). Default 0.
    pub xi: i32,
}

/// Metadata version triple kotlinc 2.4.0 stamps onto its output. We
/// emit the same so consumers running 2.4-aware reflection accept the
/// payload.
pub const METADATA_VERSION_2_4: &[i32] = &[2, 4, 0];

// ── String / qualified-name interning ──────────────────────────────

/// Insertion-order stable string table for the `d2` array, plus a
/// parallel pool of `QualifiedNameTable.QualifiedName` entries some
/// `fq_*_in_table` fields reference. Indices returned by [`intern`]
/// and [`intern_qualified`] are stable across the build.
#[derive(Default, Debug)]
pub struct StringTable {
    strings: Vec<String>,
    /// `QualifiedNameTable.QualifiedName` entries — kept for parity
    /// with kotlinc's emitter, but the reader at
    /// [`skotch_classinfo::kotlin_metadata::parse_metadata`] never
    /// consults this table (it resolves all indices straight through
    /// the string array). Phase B leaves it empty.
    qualified_names: Vec<pb::qualified_name_table::QualifiedName>,
}

impl StringTable {
    /// Intern `s`, returning its stable index. Subsequent calls with
    /// the same string return the same index.
    pub fn intern(&mut self, s: &str) -> i32 {
        if let Some(idx) = self.strings.iter().position(|t| t == s) {
            return idx as i32;
        }
        let idx = self.strings.len() as i32;
        self.strings.push(s.to_string());
        idx
    }

    /// Intern an FQ-style name (slash-separated JVM internal name).
    /// Currently delegates to [`intern`] — the kotlinc-compatible
    /// `QualifiedNameTable` walk is left for a future phase. The
    /// reader at `parse_metadata` accepts this representation: all
    /// `fq_name_id_in_table` indices route through the same
    /// `NameResolver.get_string` lookup that hits `d2` directly when
    /// no `Record` is present.
    pub fn intern_qualified(&mut self, fq: &str) -> i32 {
        self.intern(fq)
    }

    /// Snapshot the interned strings as the `d2` array.
    pub fn into_strings(self) -> Vec<String> {
        self.strings
    }

    /// Borrow the qualified-name pool (empty for Phase B).
    pub fn qualified_names(&self) -> &[pb::qualified_name_table::QualifiedName] {
        &self.qualified_names
    }
}

// ── JVM descriptor → ProtoBuf.Type ─────────────────────────────────

/// Translate a JVM type descriptor (e.g. `"Ljava/lang/String;"`, `"I"`,
/// `"[I"`) into a [`pb::Type`] whose `class_name` index points into the
/// supplied [`StringTable`]. Bare type-variable names like `"T"` are
/// not valid JVM descriptors and are routed through
/// [`type_param_to_type`] instead.
pub fn jvm_descriptor_to_type(desc: &str, table: &mut StringTable) -> pb::Type {
    let mut bytes = desc.as_bytes();
    let mut array_dims = 0u32;
    while bytes.first() == Some(&b'[') {
        array_dims += 1;
        bytes = &bytes[1..];
    }
    let element_class = match bytes.first() {
        Some(&b'V') => "kotlin/Unit",
        Some(&b'Z') => "kotlin/Boolean",
        Some(&b'B') => "kotlin/Byte",
        Some(&b'C') => "kotlin/Char",
        Some(&b'S') => "kotlin/Short",
        Some(&b'I') => "kotlin/Int",
        Some(&b'F') => "kotlin/Float",
        Some(&b'J') => "kotlin/Long",
        Some(&b'D') => "kotlin/Double",
        Some(&b'L') => {
            let rest = &bytes[1..];
            // Strip the trailing `;`.
            let end = rest.iter().position(|&b| b == b';').unwrap_or(rest.len());
            let raw = std::str::from_utf8(&rest[..end]).unwrap_or("kotlin/Any");
            return wrap_array(named_class_type(raw, table), array_dims, table);
        }
        _ => "kotlin/Any",
    };
    wrap_array(named_class_type(element_class, table), array_dims, table)
}

fn named_class_type(fq: &str, table: &mut StringTable) -> pb::Type {
    let idx = table.intern_qualified(fq);
    pb::Type {
        class_name: Some(idx),
        ..pb::Type::default()
    }
}

fn wrap_array(inner: pb::Type, dims: u32, table: &mut StringTable) -> pb::Type {
    if dims == 0 {
        return inner;
    }
    // `kotlin/Array<T>` (or just `kotlin/Array` at the outermost level
    // when nesting). For primitive element types kotlinc actually picks
    // `kotlin/IntArray` etc., but for Phase B's purposes either shape is
    // good enough — the consumer only consults the class name plus
    // generic arg count.
    let mut current = inner;
    for _ in 0..dims {
        let array_idx = table.intern_qualified("kotlin/Array");
        let mut wrapped = pb::Type {
            class_name: Some(array_idx),
            ..pb::Type::default()
        };
        wrapped.argument.push(pb::r#type::Argument {
            projection: Some(pb::r#type::argument::Projection::Inv as i32),
            r#type: Some(current),
            type_id: None,
        });
        current = wrapped;
    }
    current
}

/// Encode a type parameter (`fun <T> identity(x: T): T`) as a
/// `Type{type_parameter_name: idx}` referring to its name. The reader
/// keeps the `class_name` slot empty for these, matching what kotlinc
/// emits when the original source used a type variable.
pub fn type_param_to_type(name: &str, table: &mut StringTable) -> pb::Type {
    let idx = table.intern(name);
    pb::Type {
        type_parameter_name: Some(idx),
        ..pb::Type::default()
    }
}

/// Translate a skotch `Ty` into a `ProtoBuf.Type`. Routes primitives,
/// arrays, and classes through [`jvm_descriptor_to_type`] using their
/// JVM descriptor form; nullable wrappers set `nullable = true` on the
/// inner type. Generic parameters with arguments populate
/// `argument[]`. Function types erase to `kotlin/Any` (mirrored from
/// the JVM erasure the consumer expects).
pub fn ty_to_proto_type(ty: &Ty, table: &mut StringTable) -> pb::Type {
    match ty {
        Ty::Unit => named_class_type("kotlin/Unit", table),
        Ty::Bool => named_class_type("kotlin/Boolean", table),
        Ty::Byte => named_class_type("kotlin/Byte", table),
        Ty::Short => named_class_type("kotlin/Short", table),
        Ty::Char => named_class_type("kotlin/Char", table),
        Ty::Int => named_class_type("kotlin/Int", table),
        Ty::Float => named_class_type("kotlin/Float", table),
        Ty::Long => named_class_type("kotlin/Long", table),
        Ty::Double => named_class_type("kotlin/Double", table),
        Ty::String => named_class_type("kotlin/String", table),
        Ty::Any | Ty::Error => named_class_type("kotlin/Any", table),
        Ty::Nothing => named_class_type("kotlin/Nothing", table),
        Ty::IntArray => named_class_type("kotlin/IntArray", table),
        Ty::LongArray => named_class_type("kotlin/LongArray", table),
        Ty::DoubleArray => named_class_type("kotlin/DoubleArray", table),
        Ty::BooleanArray => named_class_type("kotlin/BooleanArray", table),
        Ty::ByteArray => named_class_type("kotlin/ByteArray", table),
        Ty::Nullable(inner) => {
            let mut t = ty_to_proto_type(inner, table);
            t.nullable = Some(true);
            t
        }
        Ty::Class(name) => named_class_type(&normalise_class_name(name), table),
        Ty::Generic { base, args } => {
            let mut t = ty_to_proto_type(base, table);
            for arg in args {
                t.argument.push(pb::r#type::Argument {
                    projection: Some(pb::r#type::argument::Projection::Inv as i32),
                    r#type: Some(ty_to_proto_type(arg, table)),
                    type_id: None,
                });
            }
            t
        }
        Ty::TypeVar(id) => type_param_to_type(&format!("T{id}"), table),
        Ty::Function { params, ret, .. } => {
            // `Function{N}<P1, P2, ..., R>` — kotlinc's stdlib name.
            let arity = params.len();
            let base = format!("kotlin/Function{arity}");
            let mut t = named_class_type(&base, table);
            for p in params {
                t.argument.push(pb::r#type::Argument {
                    projection: Some(pb::r#type::argument::Projection::Inv as i32),
                    r#type: Some(ty_to_proto_type(p, table)),
                    type_id: None,
                });
            }
            t.argument.push(pb::r#type::Argument {
                projection: Some(pb::r#type::argument::Projection::Inv as i32),
                r#type: Some(ty_to_proto_type(ret, table)),
                type_id: None,
            });
            t
        }
    }
}

/// `kotlin.String` → `kotlin/String`. Source-level class names in MIR
/// occasionally use `.` separators (typealias targets, dialect quirks);
/// the kotlinc metadata convention is slash-separated.
fn normalise_class_name(name: &str) -> String {
    name.replace('.', "/")
}

// ── Class / Function / Constructor / Property encoders ─────────────

/// Encode a single MIR function as a `ProtoBuf.Function`.
fn function_to_proto(func: &MirFunction, table: &mut StringTable) -> pb::Function {
    let name_idx = table.intern(&func.name);
    let value_parameters = collect_value_parameters(func, table);
    let return_type = Some(ty_to_proto_type(&func.return_ty, table));
    pb::Function {
        name: name_idx,
        return_type,
        value_parameter: value_parameters,
        flags: Some(function_flags(func)),
        ..pb::Function::default()
    }
}

/// Encode a MIR function as a `ProtoBuf.Constructor`. Only the value-
/// parameter list is populated — that's the field the consumer-side
/// named-arg reorder reads.
///
/// `is_secondary` controls the `IS_SECONDARY` (bit 6) flag — kotlinc
/// sets it on every body-declared `constructor(...)` so consumers can
/// tell primary from secondary at lookup time. Without it, a class
/// with no source-level primary but several explicit secondaries
/// looks (to a kotlinc consumer reading the metadata) like it has a
/// synthesized primary, which then conflicts with the matching
/// no-arg secondary the source actually declared.
fn constructor_to_proto(
    func: &MirFunction,
    table: &mut StringTable,
    is_secondary: bool,
) -> pb::Constructor {
    pb::Constructor {
        value_parameter: collect_value_parameters(func, table),
        flags: Some(constructor_flags(func, is_secondary)),
        ..pb::Constructor::default()
    }
}

fn collect_value_parameters(
    func: &MirFunction,
    table: &mut StringTable,
) -> Vec<pb::ValueParameter> {
    // The number of user-facing value parameters equals
    // `param_names.len()` (which never includes the receiver slot).
    // MIR's `params` list MAY prepend a `this` slot for instance
    // members; we detect that via the slot-count asymmetry. The
    // emitted ValueParameter list has exactly `n_user_params` entries.
    let n_user_params = func.param_names.len();
    let user_param_offset = if func.params.len() == n_user_params + 1 {
        1
    } else {
        0
    };
    let user_params: &[skotch_mir::LocalId] = if n_user_params == 0 {
        &[]
    } else if user_param_offset + n_user_params <= func.params.len() {
        &func.params[user_param_offset..user_param_offset + n_user_params]
    } else {
        // Defensive: shape mismatch — emit what we can.
        &func.params[user_param_offset..]
    };
    let mut out = Vec::with_capacity(user_params.len());
    for (i, local_id) in user_params.iter().enumerate() {
        let name = func
            .param_names
            .get(i)
            .cloned()
            .unwrap_or_else(|| format!("p{i}"));
        // Skip the trailing `$completion: Continuation` parameter that
        // mir-lower injects on suspend functions — kotlinc emits the
        // user-facing parameter list only.
        if func.is_suspend && i + 1 == user_params.len() && name == "$completion" {
            continue;
        }
        let ty = func
            .locals
            .get(local_id.0 as usize)
            .cloned()
            .unwrap_or(Ty::Any);
        // `param_defaults` is indexed against `func.params` (which
        // still includes the leading receiver slot), so the call-site
        // index is the user-list index plus the receiver offset.
        let params_idx = user_param_offset + i;
        out.push(pb::ValueParameter {
            name: table.intern(&name),
            r#type: Some(ty_to_proto_type(&ty, table)),
            flags: Some(value_parameter_flags(func, params_idx, i)),
            ..pb::ValueParameter::default()
        });
    }
    out
}

/// Encode a MIR field as a `ProtoBuf.Property`. The return-type slot
/// carries the field's declared type; getter/setter flags are left at
/// the kotlinc default (`hasGetter = true`).
fn field_to_property(field: &MirField, table: &mut StringTable) -> pb::Property {
    pb::Property {
        name: table.intern(&field.name),
        return_type: Some(ty_to_proto_type(&field.ty, table)),
        flags: Some(property_flags(field)),
        ..pb::Property::default()
    }
}

// ── Flag packing helpers ───────────────────────────────────────────
//
// kotlinc packs visibility / modality / kind / extra booleans into a
// single int32 via `Flags.getValue`. The encoding is documented in
// `org.jetbrains.kotlin.metadata.deserialization.Flags`. For Phase B
// we emit the minimal "public final declaration" shape (`6`) for all
// members, which is what the kotlinc default also resolves to. Future
// phases will plumb `is_private`, `is_open`, `is_abstract` through.

const FLAG_PUBLIC_FINAL_DECLARATION: i32 = 6;

fn function_flags(_func: &MirFunction) -> i32 {
    FLAG_PUBLIC_FINAL_DECLARATION
}

/// Constructor flag layout follows `Flags.CONSTRUCTOR_FLAGS`:
///   bit 0:    hasAnnotations
///   bits 1-3: visibility (0=internal/1=private/2=protected/3=public/…)
///   bits 4-5: modality
///   bit 6:    isSecondary
///   bit 7:    hasNonStableParameterNames
///
/// We currently emit `public final` (`6`) for every ctor; secondaries
/// additionally set the `IS_SECONDARY` bit so kotlinc consumers
/// reading the metadata don't conflate a body-declared `constructor()`
/// with the synthesized primary `<init>()V` (parity/101-hash MD5).
/// `func.is_private` flips the visibility bits from public to
/// private, mirroring kotlinc's `0x12` shape for
/// `private constructor(...)`.
fn constructor_flags(func: &MirFunction, is_secondary: bool) -> i32 {
    const IS_SECONDARY_BIT: i32 = 1 << 6;
    let visibility = if func.is_private {
        pb::Visibility::Private as i32 // 0
    } else {
        pb::Visibility::Public as i32 // 3
    };
    // Layout: hasAnnotations(1) | visibility(3) | modality(2) | ...
    // `FLAG_PUBLIC_FINAL_DECLARATION` (6) = (Public << 1) | (Final << 4).
    let mut flags = (visibility << 1) | ((pb::Modality::Final as i32) << 4);
    if is_secondary {
        flags |= IS_SECONDARY_BIT;
    }
    flags
}

fn value_parameter_flags(func: &MirFunction, params_idx: usize, user_idx: usize) -> i32 {
    // ValueParameter flag layout (see kotlinc's
    // `Flags.VALUE_PARAMETER_FLAGS`):
    //   bit 0: hasAnnotations
    //   bit 1: declaresDefault
    //   bit 2: isCrossinline
    //   bit 3: isNoinline
    //
    // We can only recover `declaresDefault` from MIR right now. Two
    // sources, in priority order:
    //   1. `param_defaults[idx]` is `Some(MirConst)` — mir-lower
    //      lowered a literal default and the backend will emit a
    //      `<name>$default` synthetic shim.
    //   2. `required_params` says "the first N user params are
    //      required"; user params at index >= required_params have a
    //      default. Constructors whose mir-lower path leaves
    //      `param_defaults` empty (e.g. `cause: Throwable? = null`
    //      where the default lowering is implicit) still set
    //      `required_params < user_count`, so this is the
    //      consumer-side signal kotlinc relies on for named-arg
    //      reordering across skipped defaults.
    //
    // Without this bit kotlinc 2.4 refuses calls like
    // `CliktError(msg = "x")` with "no value passed for parameter
    // 'cause'".
    let mut flags = 0i32;
    let has_literal_default = matches!(func.param_defaults.get(params_idx), Some(Some(_)));
    // Treat `required_params == 0` as "no defaults known" only when
    // `param_names` is also empty (caller didn't populate either) —
    // otherwise zero required-params means "ALL params have defaults",
    // which is the shape mir-lower produces for `class C(val x = 1,
    // val y = 2)` (see `constructor_from_primary_impl`).
    let n_user_params = func.param_names.len().max(func.params.len());
    let any_user_params = n_user_params > 0;
    let has_implicit_default =
        any_user_params && func.required_params < n_user_params && user_idx >= func.required_params;
    if has_literal_default || has_implicit_default {
        flags |= 1 << 1;
    }
    flags
}

fn property_flags(_field: &MirField) -> i32 {
    // Bit 9 = hasGetter, plus the standard public/final tail.
    518
}

/// Compute the class-level flags. Mirrors kotlinc's packing in
/// `Flags.CLASS_FLAGS` — for Phase B we only differentiate the
/// `Kind` field (CLASS / INTERFACE / OBJECT / etc.) on top of the
/// `public final` default.
fn class_flags(class: &MirClass) -> i32 {
    let kind = if class.is_interface {
        pb::class::Kind::Interface as i32
    } else if class.is_object_singleton {
        pb::class::Kind::Object as i32
    } else {
        pb::class::Kind::Class as i32
    };
    // Layout (LSB → MSB): hasAnnotations(1) | visibility(3) | modality(2) | kind(3) | ...
    // We approximate kotlinc's `FLAGS_DEFAULT` for `public final class` and
    // override the kind bits.
    let visibility_public = pb::Visibility::Public as i32; // 3
    let modality_final = pb::Modality::Final as i32; // 0
    (visibility_public << 1) | (modality_final << 4) | (kind << 6)
}

// ── Top-level entry points ─────────────────────────────────────────

/// Produce a [`pb::Class`] for a single MIR class, populating the
/// fq_name / constructor / function / property fields against the
/// supplied [`StringTable`]. Companion-object metadata is recorded
/// via `companion_object_name` when present; the companion class
/// itself is expected to be encoded into its own `Metadata` payload
/// by a separate call.
pub fn class_proto_for(class: &MirClass, table: &mut StringTable) -> pb::Class {
    let fq_name = table.intern_qualified(&class.name);
    let mut out = pb::Class {
        flags: Some(class_flags(class)),
        fq_name,
        ..pb::Class::default()
    };
    // Primary constructor first, then secondaries — matches kotlinc's
    // declaration order so the cross-file named-arg reorder picks the
    // primary by index 0.
    //
    // Exception: when the Kotlin source declared NO primary constructor
    // (`class MD5 : Digest { constructor(): super(...) {} ... }`),
    // mir-lower still synthesizes an empty no-arg
    // [`MirClass::constructor`] for backend-uniformity. Emitting that
    // shell into `@Metadata` next to a body-level `constructor()`
    // duplicates the `<init>()V` proto, which kotlinc 2.4 rejects with
    // "overload resolution ambiguity" — and the cascading
    // unresolved-reference diagnostics that follow (`update`,
    // `digest`, `blockSize`, …) blocked parity/101-hash. Suppress the
    // synthesized shell in that exact shape: source has no primary AND
    // at least one explicit secondary covers the same call surface.
    let suppress_synthesized_primary =
        !class.has_explicit_primary_ctor && !class.secondary_constructors.is_empty();
    if !suppress_synthesized_primary {
        out.constructor
            .push(constructor_to_proto(&class.constructor, table, false));
    }
    for sec in &class.secondary_constructors {
        out.constructor.push(constructor_to_proto(sec, table, true));
    }
    for m in &class.methods {
        // Constructors live in their own slot; abstract methods still
        // carry a Function entry so consumers can see the signature.
        out.function.push(function_to_proto(m, table));
    }
    for f in &class.fields {
        out.property.push(field_to_property(f, table));
    }
    for f in &class.static_fields {
        out.property.push(field_to_property(f, table));
    }
    if let Some(companion) = &class.companion_class_name {
        out.companion_object_name = Some(table.intern(companion));
    }
    if !class.interfaces.is_empty() {
        for iface in &class.interfaces {
            out.supertype.push(named_class_type(iface, table));
        }
    }
    if let Some(sc) = &class.super_class {
        out.supertype.push(named_class_type(sc, table));
    }
    out
}

/// Produce a [`pb::Package`] for the file-facade (`*Kt`) wrapper class
/// of a MIR module. Top-level functions, props and consts are encoded
/// here; user-declared classes get their own `pb::Class` payloads via
/// [`class_proto_for`].
pub fn package_proto_for(module: &MirModule, table: &mut StringTable) -> pb::Package {
    let mut out = pb::Package::default();
    for f in &module.functions {
        out.function.push(function_to_proto(f, table));
    }
    for (name, ty, _value) in &module.top_level_props {
        out.property.push(pb::Property {
            name: table.intern(name),
            return_type: Some(ty_to_proto_type(ty, table)),
            flags: Some(518),
            ..pb::Property::default()
        });
    }
    for (name, ty, _value) in &module.top_level_consts {
        out.property.push(pb::Property {
            name: table.intern(name),
            return_type: Some(ty_to_proto_type(ty, table)),
            // Const flag set: bit 8 (isConst = true) on top of the
            // hasGetter default — keeps consumer-side `is const`
            // detection round-tripping.
            flags: Some(518 | (1 << 8)),
            ..pb::Property::default()
        });
    }
    out
}

// ── End-to-end Metadata encoding ───────────────────────────────────

/// Encode a single MIR class as a kotlinc-style class-metadata payload
/// (`@Metadata.k = 1`). `module_name` is the JVM module the class
/// belongs to (defaults to `"main"` for stdlib parity); unused by the
/// current reader but kept in the signature for future
/// `JvmMetadata.class_module_name` plumbing.
pub fn encode_class_metadata(class: &MirClass, _module_name: &str) -> Metadata {
    let mut table = StringTable::default();
    let proto = class_proto_for(class, &mut table);
    let body = proto.encode_to_vec();
    let d2 = table.into_strings();
    let d1 = encode_payload(body, d2.len());
    Metadata {
        k: 1,
        mv: METADATA_VERSION_2_4.to_vec(),
        d1,
        d2,
        ..Metadata::default()
    }
}

/// Encode a MIR module's file facade (`*Kt`) as a kotlinc-style
/// package-metadata payload (`@Metadata.k = 2`).
pub fn encode_package_metadata(module: &MirModule) -> Metadata {
    let mut table = StringTable::default();
    let proto = package_proto_for(module, &mut table);
    let body = proto.encode_to_vec();
    let d2 = table.into_strings();
    let d1 = encode_payload(body, d2.len());
    Metadata {
        k: 2,
        mv: METADATA_VERSION_2_4.to_vec(),
        d1,
        d2,
        ..Metadata::default()
    }
}

/// Prepend a length-delimited `StringTableTypes` to the
/// `Class`/`Package` body and bit-encode the resulting byte stream.
/// The reader at [`skotch_classinfo::kotlin_metadata::parse_metadata`]
/// consumes exactly that shape: `read_len_bytes` for the leading
/// StringTableTypes, then `remaining()` for the body.
///
/// The `StringTableTypes` is built with `string_count` "default"
/// records collapsed into a single `Record { range = N }`. kotlinc's
/// `JvmNameResolverKt.toExpandedRecordsList` expands that into N empty
/// records whose `getString(i)` falls through to `strings[i]` — i.e.
/// the d2 entry — which is the value-paramater-name / class-name our
/// `StringTable` already populated d2 with.
fn encode_payload(body: Vec<u8>, string_count: usize) -> Vec<String> {
    let mut combined = Vec::new();
    let stt = build_string_table_types(string_count);
    let stt_bytes = stt.encode_to_vec();
    // Length-delimited (varint length + bytes).
    encode_varint(stt_bytes.len() as u64, &mut combined);
    combined.extend_from_slice(&stt_bytes);
    combined.extend_from_slice(&body);
    encode_bytes(&combined)
}

/// Build a `StringTableTypes` message whose expanded records list
/// covers indices `0..string_count`. Each expanded record is "default"
/// (no `string`, no `predefined_index`, no `operation`/`substring`/
/// `replace_char`), so `JvmNameResolverBase.getString(i)` falls
/// through to the `d2` array at `i`. Encoded as a single record with
/// `range = string_count` to keep the wire form compact.
fn build_string_table_types(string_count: usize) -> jvm_pb::StringTableTypes {
    let mut stt = jvm_pb::StringTableTypes::default();
    if string_count > 0 {
        stt.record.push(jvm_pb::string_table_types::Record {
            range: Some(string_count as i32),
            ..jvm_pb::string_table_types::Record::default()
        });
    }
    stt
}

fn encode_varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
            out.push(byte);
        } else {
            out.push(byte);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_classinfo::kotlin_metadata::{
        bit_encoding::decode_bytes, parse_metadata, RawMetadata,
    };
    use skotch_mir::{BasicBlock, FuncId, LocalId, MirField, MirFunction, Terminator};

    fn mk_function(
        name: &str,
        return_ty: Ty,
        params: Vec<(&str, Ty)>,
        next_id: u32,
    ) -> MirFunction {
        let mut locals = Vec::new();
        let mut param_ids = Vec::new();
        let mut param_names = Vec::new();
        for (pname, pty) in params {
            let id = LocalId(locals.len() as u32);
            locals.push(pty);
            param_ids.push(id);
            param_names.push(pname.to_string());
        }
        MirFunction {
            id: FuncId(next_id),
            name: name.to_string(),
            params: param_ids,
            locals,
            blocks: vec![BasicBlock {
                stmts: vec![],
                terminator: Terminator::Return,
            }],
            return_ty,
            required_params: 0,
            param_names,
            param_receiver_types: vec![],
            param_defaults: vec![],
            is_abstract: false,
            vararg_index: None,
            exception_handlers: vec![],
            is_suspend: false,
            is_inline: false,
            is_tailrec: false,
            has_type_params: false,
            suspend_original_return_ty: None,
            suspend_state_machine: None,
            annotations: vec![],
            named_locals: vec![],
            is_private: false,
            is_static: false,
            default_call_masks: vec![],
            needs_leading_nop: false,
            local_generic_args: rustc_hash::FxHashMap::default(),
        }
    }

    /// Like [`mk_function`] but prepends a synthetic `this` slot to
    /// `params` (without naming it in `param_names`). Models the
    /// real-world MIR shape for instance methods / constructors:
    /// `params.len() == param_names.len() + 1`. The walker uses that
    /// asymmetry to detect-and-skip the receiver slot.
    fn mk_instance_function(
        name: &str,
        receiver_ty: Ty,
        return_ty: Ty,
        params: Vec<(&str, Ty)>,
        next_id: u32,
    ) -> MirFunction {
        let mut locals = vec![receiver_ty];
        let mut param_ids = vec![LocalId(0)];
        let mut param_names = Vec::new();
        for (pname, pty) in params {
            let id = LocalId(locals.len() as u32);
            locals.push(pty);
            param_ids.push(id);
            param_names.push(pname.to_string());
        }
        MirFunction {
            id: FuncId(next_id),
            name: name.to_string(),
            params: param_ids,
            locals,
            blocks: vec![BasicBlock {
                stmts: vec![],
                terminator: Terminator::Return,
            }],
            return_ty,
            required_params: 0,
            param_names,
            param_receiver_types: vec![],
            param_defaults: vec![],
            is_abstract: false,
            vararg_index: None,
            exception_handlers: vec![],
            is_suspend: false,
            is_inline: false,
            is_tailrec: false,
            has_type_params: false,
            suspend_original_return_ty: None,
            suspend_state_machine: None,
            annotations: vec![],
            named_locals: vec![],
            is_private: false,
            is_static: false,
            default_call_masks: vec![],
            needs_leading_nop: false,
            local_generic_args: rustc_hash::FxHashMap::default(),
        }
    }

    fn mk_class(name: &str, ctor_params: Vec<(&str, Ty)>) -> MirClass {
        // Real-shape constructor: `params[0]` is `this` (the class
        // itself), and `param_names` only lists user-facing params.
        let ctor = mk_instance_function(
            "<init>",
            Ty::Class(name.to_string()),
            Ty::Unit,
            ctor_params.clone(),
            0,
        );
        let fields = ctor_params
            .iter()
            .map(|(n, ty)| MirField {
                name: n.to_string(),
                ty: ty.clone(),
                is_jvm_field: false,
            })
            .collect();
        MirClass {
            name: name.to_string(),
            super_class: None,
            is_open: false,
            is_abstract: false,
            is_interface: false,
            interfaces: vec![],
            fields,
            methods: vec![],
            constructor: ctor,
            secondary_constructors: vec![],
            has_explicit_primary_ctor: true,
            is_suspend_lambda: false,
            is_lambda: false,
            is_cross_file_stub: false,
            annotations: vec![],
            has_type_params: false,
            is_object_singleton: false,
            companion_class_name: None,
            static_fields: vec![],
            clinit: None,
        }
    }

    /// Phase B acceptance test: encode a one-parameter class through
    /// the writer, decode via the existing skotch-classinfo reader,
    /// and assert the round-trip preserves the constructor parameter.
    #[test]
    fn class_with_single_ctor_param_round_trips() {
        let class = mk_class("Foo", vec![("x", Ty::Int)]);
        let md = encode_class_metadata(&class, "main");
        assert_eq!(md.k, 1);
        assert_eq!(md.mv, vec![2, 4, 0]);

        let raw = RawMetadata {
            kind: md.k,
            data1: md.d1.clone(),
            data2: md.d2.clone(),
        };
        let parsed = parse_metadata(&raw).expect("parse Foo metadata");
        assert_eq!(parsed.constructors.len(), 1);
        let params = &parsed.constructors[0].value_params;
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "x");
        assert_eq!(params[0].ty.class_name.as_deref(), Some("kotlin/Int"));
    }

    /// 100-clikt-shaped test: three named constructor parameters,
    /// each with a different type (String, nullable Throwable, Int).
    /// The named-arg reorder consumer needs ALL three names to
    /// survive the round trip in declaration order.
    #[test]
    fn class_with_three_named_ctor_params_round_trips() {
        let class = mk_class(
            "CliktError",
            vec![
                ("msg", Ty::String),
                (
                    "cause",
                    Ty::Nullable(Box::new(Ty::Class("Throwable".into()))),
                ),
                ("statusCode", Ty::Int),
            ],
        );
        let md = encode_class_metadata(&class, "main");
        let raw = RawMetadata {
            kind: md.k,
            data1: md.d1,
            data2: md.d2,
        };
        let parsed = parse_metadata(&raw).expect("parse CliktError metadata");
        assert_eq!(parsed.constructors.len(), 1);
        let names: Vec<&str> = parsed.constructors[0]
            .value_params
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["msg", "cause", "statusCode"]);
        // Spot-check the type lookups too.
        let tys = &parsed.constructors[0].value_params;
        assert_eq!(tys[0].ty.class_name.as_deref(), Some("kotlin/String"));
        assert!(tys[1].ty.nullable, "cause must round-trip as nullable");
        assert_eq!(tys[1].ty.class_name.as_deref(), Some("Throwable"));
        assert_eq!(tys[2].ty.class_name.as_deref(), Some("kotlin/Int"));
    }

    /// File-facade (`k = 2`) metadata: a single top-level function
    /// `fun greet(name: String): Unit`. The Package proto must
    /// surface the function's name and parameter list.
    #[test]
    fn package_with_top_level_fn_round_trips() {
        let mut module = MirModule {
            wrapper_class: "HelloKt".to_string(),
            ..MirModule::default()
        };
        module.functions.push(mk_function(
            "greet",
            Ty::Unit,
            vec![("name", Ty::String)],
            0,
        ));

        let md = encode_package_metadata(&module);
        assert_eq!(md.k, 2);

        let raw = RawMetadata {
            kind: md.k,
            data1: md.d1,
            data2: md.d2,
        };
        let parsed = parse_metadata(&raw).expect("parse Hello facade");
        assert!(parsed.constructors.is_empty(), "k=2 has no constructors");
        assert_eq!(parsed.functions.len(), 1);
        let f = &parsed.functions[0];
        assert_eq!(f.name, "greet");
        assert_eq!(f.value_params.len(), 1);
        assert_eq!(f.value_params[0].name, "name");
        assert_eq!(
            f.value_params[0].ty.class_name.as_deref(),
            Some("kotlin/String")
        );
        assert_eq!(
            f.return_type.as_ref().and_then(|t| t.class_name.as_deref()),
            Some("kotlin/Unit")
        );
    }

    /// Functions with multiple value parameters and a non-Unit return
    /// type — exercises the `Function.return_type` path on the
    /// consumer side and asserts parameter order is stable.
    #[test]
    fn class_method_with_return_type_round_trips() {
        let mut class = mk_class("Greeter", vec![]);
        class.methods.push(mk_instance_function(
            "greet",
            Ty::Class("Greeter".to_string()),
            Ty::String,
            vec![("first", Ty::String), ("count", Ty::Int)],
            1,
        ));
        let md = encode_class_metadata(&class, "main");
        let raw = RawMetadata {
            kind: md.k,
            data1: md.d1,
            data2: md.d2,
        };
        let parsed = parse_metadata(&raw).expect("parse Greeter metadata");
        assert_eq!(parsed.functions.len(), 1);
        let f = &parsed.functions[0];
        assert_eq!(f.name, "greet");
        let names: Vec<&str> = f.value_params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["first", "count"]);
        assert_eq!(
            f.return_type.as_ref().and_then(|t| t.class_name.as_deref()),
            Some("kotlin/String")
        );
    }

    /// Sanity-check the JVM-descriptor lowering: arrays nest a
    /// `kotlin/Array<Inner>` wrapper for every leading `[`.
    #[test]
    fn jvm_descriptor_basics() {
        let mut t = StringTable::default();
        let int_ty = jvm_descriptor_to_type("I", &mut t);
        assert_eq!(
            t.into_strings().get(int_ty.class_name.unwrap() as usize),
            Some(&"kotlin/Int".to_string())
        );

        let mut t = StringTable::default();
        let str_ty = jvm_descriptor_to_type("Ljava/lang/String;", &mut t);
        assert_eq!(
            t.into_strings().get(str_ty.class_name.unwrap() as usize),
            Some(&"java/lang/String".to_string())
        );

        let mut t = StringTable::default();
        let arr_ty = jvm_descriptor_to_type("[I", &mut t);
        assert_eq!(arr_ty.argument.len(), 1);
        let strings = t.into_strings();
        assert_eq!(
            strings.get(arr_ty.class_name.unwrap() as usize),
            Some(&"kotlin/Array".to_string())
        );
        let inner = arr_ty.argument[0]
            .r#type
            .as_ref()
            .and_then(|t| t.class_name);
        assert_eq!(
            inner.and_then(|i| strings.get(i as usize).cloned()),
            Some("kotlin/Int".to_string())
        );
    }

    /// Bit-encoding sanity: the Phase A round-trip path must accept
    /// our Phase D output unchanged. Phase D emits a non-empty
    /// `StringTableTypes` (a single `Record { range = d2.len() }`) so
    /// kotlinc's strict `JvmNameResolverBase` doesn't IOOB on the
    /// records list.
    #[test]
    fn bit_encoding_round_trips_through_decoder() {
        let class = mk_class("Foo", vec![("x", Ty::Int)]);
        let md = encode_class_metadata(&class, "main");
        // Decoded `d1` bytes must equal a length-prefixed
        // StringTableTypes followed by the Class body — verify the
        // prefix length parses and the remaining bytes are non-empty.
        let bytes = decode_bytes(&md.d1);
        assert!(bytes.len() > 1);
        // First byte is the varint length of the StringTableTypes
        // message. Phase D emits exactly one record (`range = N`),
        // which serialises to a small (>0) byte count.
        assert!(bytes[0] > 0, "expected non-empty StringTableTypes prefix");
    }

    /// Secondary constructors must surface as separate `Constructor`
    /// entries on the `Class` proto, in declaration order.
    #[test]
    fn secondary_constructors_round_trip() {
        let mut class = mk_class("Box", vec![("x", Ty::Int)]);
        class.secondary_constructors.push(mk_instance_function(
            "<init>",
            Ty::Class("Box".to_string()),
            Ty::Unit,
            vec![("y", Ty::String)],
            0,
        ));
        let md = encode_class_metadata(&class, "main");
        let raw = RawMetadata {
            kind: md.k,
            data1: md.d1,
            data2: md.d2,
        };
        let parsed = parse_metadata(&raw).expect("parse Box metadata");
        assert_eq!(parsed.constructors.len(), 2);
        assert_eq!(parsed.constructors[0].value_params[0].name, "x");
        assert_eq!(parsed.constructors[1].value_params[0].name, "y");
        assert_eq!(
            parsed.constructors[1].value_params[0]
                .ty
                .class_name
                .as_deref(),
            Some("kotlin/String")
        );
    }

    /// `class Greeter` plus a top-level `fun greet(name: String)` —
    /// confirms a single `MirModule` can produce both flavours of
    /// payload without state leakage between the two `StringTable`s.
    #[test]
    fn module_emits_class_and_package_independently() {
        let mut module = MirModule {
            wrapper_class: "GreetingKt".to_string(),
            ..MirModule::default()
        };
        module.functions.push(mk_function(
            "salute",
            Ty::String,
            vec![("who", Ty::String)],
            0,
        ));
        let class = mk_class("Greeter", vec![("name", Ty::String)]);

        let md_class = encode_class_metadata(&class, "main");
        let md_pkg = encode_package_metadata(&module);
        assert_eq!(md_class.k, 1);
        assert_eq!(md_pkg.k, 2);
        assert_ne!(md_class.d2, md_pkg.d2, "string tables are per-payload");
    }

    /// Skipping suppression: a `suspend fun` carries a trailing
    /// `$completion` parameter in MIR, but kotlinc's user-facing
    /// metadata excludes it. We mirror that.
    #[test]
    fn suspend_completion_param_is_dropped() {
        let mut module = MirModule::default();
        let mut f = mk_function(
            "fetch",
            Ty::Any,
            vec![("url", Ty::String), ("$completion", Ty::Any)],
            0,
        );
        f.is_suspend = true;
        module.functions.push(f);
        let md = encode_package_metadata(&module);
        let raw = RawMetadata {
            kind: md.k,
            data1: md.d1,
            data2: md.d2,
        };
        let parsed = parse_metadata(&raw).expect("parse facade");
        assert_eq!(parsed.functions[0].value_params.len(), 1);
        assert_eq!(parsed.functions[0].value_params[0].name, "url");
    }
}
