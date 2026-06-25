//! Phase H2: `@JvmInline value class` synthetic-method emission.
//!
//! For a class flagged `is_value_class` in MIR (see Phase H1 detection
//! in `skotch-mir-lower::detect_value_class_underlying`), kotlinc emits
//! a particular shape that we mirror here:
//!
//! - `private synthetic <init>(<U>)V` — the real ctor (becomes ACC_PRIVATE
//!   so external callers must route through `box-impl`).
//! - `public static constructor-impl(<U>): <U>` — identity (any user
//!   `init` block validation would land here in a future phase).
//! - `public static synthetic box-impl(<U>): V` — wraps the underlying
//!   value in a freshly-allocated `V` (calls the private `<init>`).
//! - `public synthetic unbox-impl(): <U>` — extracts the underlying
//!   value (`getfield this.<field>`).
//! - `public static equals-impl(<U>, Object): Z` — null-aware equality;
//!   unboxes the other side and delegates to `equals-impl0`.
//! - `public static equals-impl0(<U>, <U>): Z` — direct two-underlying
//!   equality, suitable for inlining at known-shape call sites.
//! - `public static hashCode-impl(<U>): I` — boxes-and-hashes the
//!   underlying value (or `<U>.hashCode()` for primitives).
//! - `public static toString-impl(<U>): String` — `V(field=<U>)`-shaped
//!   render mirroring kotlinc's data-class toString.
//!
//! The existing instance methods (`equals(Object)Z`, `hashCode()I`,
//! `toString()Ljava/lang/String;`) on the value class become thin
//! forwarders that getfield the underlying value and tail-call the
//! static `-impl` variant.
//!
//! User-declared instance methods also get a `<name>[-mangle]-impl(<U>,
//! ...): R` static variant whose body is the original method's body but
//! with the implicit `this` rewritten as a load of the static `arg0`
//! followed by a `box-impl` boxing — a degenerate but correct shape
//! that lets H3 optimize call sites to skip both unbox AND box once the
//! call-site rewrite phase lands. The static variant's name carries the
//! KEEP-104 mangling hash if and only if at least one parameter is itself
//! a value-class type.
//!
//! See [`mangle_method_name`] for the exact mangling spec (cross-checked
//! against `org.jetbrains.kotlin.codegen.state.InlineClassManglingUtilsKt`
//! decompiled from `kotlin-compiler.jar`).

use byteorder::{BigEndian, WriteBytesExt};
use md5::{Digest, Md5};
use skotch_types::Ty;
use std::io::Write;

use crate::constant_pool::ConstantPool;

const ACC_PUBLIC: u16 = 0x0001;
const ACC_STATIC: u16 = 0x0008;
const ACC_FINAL: u16 = 0x0010;
const ACC_SYNTHETIC: u16 = 0x1000;

/// A value-class parameter description, used as input to KEEP-104
/// mangling. `is_value` is true when the source-level parameter type
/// is itself a `@JvmInline value class` (`fq_name` then is the JVM
/// internal name of that class, e.g. `"com/example/UserId"`).
/// Non-value-class parameters contribute the literal `"_"` placeholder
/// in the signature string; their `fq_name` is ignored.
#[derive(Debug, Clone)]
pub struct MangleParam {
    /// Fully qualified JVM-internal name of the parameter's value-class
    /// type (only consulted when `is_value` is true). The mangling
    /// expects the **dotted** kotlin FQ-name (`com.example.UserId`),
    /// produced from the JVM internal name by replacing `/` with `.`.
    pub fq_name: String,
    /// True when the parameter's source type is a value class.
    pub is_value: bool,
    /// True when the parameter type carries a `?` (nullable). Affects
    /// only the rendered mangling element — `Lfoo/Bar?;` vs `Lfoo/Bar;`.
    pub is_nullable: bool,
}

// `mangle_method_name` + `primitive_descriptor_char` are public for
// future call-site rewriting (Phase H3 inserts mangled `invokestatic`
// calls that bypass the wrapper instance methods). They are exercised
// by the test suite below; suppress the dead-code warning until H3
// lights them up at the call site.
#[allow(dead_code)]
/// Compute the KEEP-104 mangling suffix (Base64URL-without-padding of
/// the first 5 bytes of MD5(signatureForMangling)) for a method on a
/// `@JvmInline value class`.
///
/// Returns `None` if the method should NOT be mangled — namely when
/// none of its value parameters are themselves value classes. (kotlinc
/// only mangles methods that "see" a value-class type through their
/// parameter list; pure-primitive methods get the plain `-impl` shape.)
///
/// Spec — cross-checked against `org.jetbrains.kotlin.codegen.state
/// .InlineClassManglingUtilsKt` (kotlin-compiler.jar):
///
/// ```text
/// signatureForMangling := join("", [render(p) for p in params])
/// render(p)            := if p.is_value: "L" + p.fq_name + (if p.is_nullable: "?") + ";"
///                         else: "_"
/// hash                 := base64url_nopad(md5(signatureForMangling)[..5])
/// suffix               := "-" + hash       // 7 base64 chars → 8-char suffix
/// ```
pub fn mangle_method_name(name: &str, params: &[MangleParam]) -> Option<String> {
    // Only mangle when at least one param is itself a value class.
    // Methods like `fun double(): Long` on `value class V(val x: Long)`
    // — zero params, all-primitive — get plain `"double-impl"`.
    if !params.iter().any(|p| p.is_value) {
        return None;
    }
    let mut sig = String::new();
    for p in params {
        if p.is_value {
            sig.push('L');
            sig.push_str(&p.fq_name);
            if p.is_nullable {
                sig.push('?');
            }
            sig.push(';');
        } else {
            sig.push('_');
        }
    }
    let mut hasher = Md5::new();
    hasher.update(sig.as_bytes());
    let digest = hasher.finalize();
    let suffix = base64url_no_pad(&digest[..5]);
    Some(format!("{name}-{suffix}"))
}

#[allow(dead_code)]
/// Minimal URL-safe Base64 encoder without padding. Matches
/// `java.util.Base64.getUrlEncoder().withoutPadding()` for short
/// inputs (5 bytes → 7 chars). Inlined to avoid pulling in the
/// `base64` crate for a single 5-byte call site.
fn base64url_no_pad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() * 4 + 2) / 3);
    let chunks = bytes.chunks(3);
    for chunk in chunks {
        match chunk.len() {
            3 => {
                let b0 = chunk[0] as u32;
                let b1 = chunk[1] as u32;
                let b2 = chunk[2] as u32;
                let n = (b0 << 16) | (b1 << 8) | b2;
                out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
                out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
                out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
                out.push(ALPHABET[(n & 0x3F) as usize] as char);
            }
            2 => {
                let b0 = chunk[0] as u32;
                let b1 = chunk[1] as u32;
                let n = (b0 << 16) | (b1 << 8);
                out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
                out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
                out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
            }
            1 => {
                let b0 = chunk[0] as u32;
                let n = b0 << 16;
                out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
                out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
            }
            _ => {}
        }
    }
    out
}

#[allow(dead_code)]
/// JVM descriptor letter for a primitive underlying value-class type.
/// Returns `None` for non-primitive underlyings (e.g. value classes
/// over `String` — `Ljava/lang/String;` is its descriptor, not a single
/// letter).
pub fn primitive_descriptor_char(ty: &Ty) -> Option<char> {
    match ty {
        Ty::Bool => Some('Z'),
        Ty::Byte => Some('B'),
        Ty::Short => Some('S'),
        Ty::Char => Some('C'),
        Ty::Int => Some('I'),
        Ty::Float => Some('F'),
        Ty::Long => Some('J'),
        Ty::Double => Some('D'),
        _ => None,
    }
}

/// JVM descriptor for the underlying type. e.g. `Ty::Long` → `"J"`,
/// `Ty::String` → `"Ljava/lang/String;"`. Mirrors the subset of
/// `jvm_type_string` we need for value-class synthetics (the full
/// version lives in `class_writer.rs`, but we avoid coupling to it
/// to keep this module independently testable).
pub fn underlying_descriptor(ty: &Ty) -> String {
    match ty {
        Ty::Bool => "Z".to_string(),
        Ty::Byte => "B".to_string(),
        Ty::Short => "S".to_string(),
        Ty::Char => "C".to_string(),
        Ty::Int => "I".to_string(),
        Ty::Float => "F".to_string(),
        Ty::Long => "J".to_string(),
        Ty::Double => "D".to_string(),
        Ty::String => "Ljava/lang/String;".to_string(),
        Ty::Class(n) => format!("L{n};"),
        Ty::Any => "Ljava/lang/Object;".to_string(),
        Ty::IntArray => "[I".to_string(),
        Ty::LongArray => "[J".to_string(),
        Ty::DoubleArray => "[D".to_string(),
        Ty::BooleanArray => "[Z".to_string(),
        Ty::ByteArray => "[B".to_string(),
        // Other Ty variants (Nullable, Generic, Function, …) aren't
        // expected as value-class underlying types — kotlinc rejects
        // them at the source level. Fall back to Object so we emit a
        // well-formed (if generic) descriptor rather than panic.
        _ => "Ljava/lang/Object;".to_string(),
    }
}

/// Slot width of a JVM type — 2 for Long/Double, 1 otherwise. Mirrors
/// the JVMS §2.6.1 "category 2" types. Used to compute parameter slot
/// offsets in the synthetic method bodies.
fn slot_width(ty: &Ty) -> u16 {
    if matches!(ty, Ty::Long | Ty::Double) {
        2
    } else {
        1
    }
}

/// Return opcode for a given underlying type.
fn return_op(ty: &Ty) -> u8 {
    match ty {
        Ty::Bool | Ty::Byte | Ty::Short | Ty::Char | Ty::Int => 0xAC, // ireturn
        Ty::Long => 0xAD,                                             // lreturn
        Ty::Float => 0xAE,                                            // freturn
        Ty::Double => 0xAF,                                           // dreturn
        Ty::Unit => 0xB1,                                             // return
        _ => 0xB0,                                                    // areturn
    }
}

fn emit_load(code: &mut Vec<u8>, ty: &Ty, slot: u8) {
    // Use the short Xload_N form for slots 0..=3 to mirror kotlinc.
    match ty {
        Ty::Bool | Ty::Byte | Ty::Short | Ty::Char | Ty::Int => {
            if slot <= 3 {
                code.push(0x1A + slot); // iload_0..iload_3
            } else {
                code.push(0x15);
                code.push(slot);
            }
        }
        Ty::Long => {
            if slot <= 3 {
                code.push(0x1E + slot); // lload_0..lload_3
            } else {
                code.push(0x16);
                code.push(slot);
            }
        }
        Ty::Float => {
            if slot <= 3 {
                code.push(0x22 + slot); // fload_0..fload_3
            } else {
                code.push(0x17);
                code.push(slot);
            }
        }
        Ty::Double => {
            if slot <= 3 {
                code.push(0x26 + slot); // dload_0..dload_3
            } else {
                code.push(0x18);
                code.push(slot);
            }
        }
        _ => {
            if slot <= 3 {
                code.push(0x2A + slot); // aload_0..aload_3
            } else {
                code.push(0x19);
                code.push(slot);
            }
        }
    }
}

/// Build a method blob with a single Code attribute. `code` is the raw
/// instruction stream. `max_stack` / `max_locals` are caller-computed.
/// `code_attr_name_idx` is the constant-pool Utf8 index for `"Code"`
/// (registered once by the surrounding class emitter). Pass a non-empty
/// `stack_map_table` to attach a `StackMapTable` Code sub-attribute —
/// the bytes are the **entry list only** (the caller computes the
/// frames; `build_method_blob` prepends `number_of_entries` and the
/// attribute header). When `None`, the Code attribute has zero
/// sub-attributes, which is valid for branch-free methods only. The
/// caller is responsible for registering `"StackMapTable"` in the
/// constant pool ahead of time (and passing its index via the
/// `StackMapTable.name_idx` field) so the sub-attribute name doesn't
/// require post-emission patching — value-class synthetics are emitted
/// AFTER the user methods + getters, so the class-writer's CP-late-
/// registration phase has already settled by the time we land here.
fn build_method_blob(
    access_flags: u16,
    name_idx: u16,
    descriptor_idx: u16,
    max_stack: u16,
    max_locals: u16,
    code: &[u8],
    code_attr_name_idx: u16,
    stack_map_table: Option<StackMapTable>,
) -> Vec<u8> {
    let smt_section_len: u32 = stack_map_table
        .as_ref()
        .map(|smt| 2 + 4 + 2 + smt.entries.len() as u32)
        .unwrap_or(0);
    let sub_attr_count: u16 = if stack_map_table.is_some() { 1 } else { 0 };

    let mut blob = Vec::with_capacity(32 + code.len() + smt_section_len as usize);
    blob.write_u16::<BigEndian>(access_flags).unwrap();
    blob.write_u16::<BigEndian>(name_idx).unwrap();
    blob.write_u16::<BigEndian>(descriptor_idx).unwrap();
    blob.write_u16::<BigEndian>(1).unwrap(); // attributes_count = 1 (Code only)
    blob.write_u16::<BigEndian>(code_attr_name_idx).unwrap();
    let attr_len: u32 = 2 + 2 + 4 + (code.len() as u32) + 2 + 2 + smt_section_len;
    blob.write_u32::<BigEndian>(attr_len).unwrap();
    blob.write_u16::<BigEndian>(max_stack).unwrap();
    blob.write_u16::<BigEndian>(max_locals).unwrap();
    blob.write_u32::<BigEndian>(code.len() as u32).unwrap();
    blob.write_all(code).unwrap();
    blob.write_u16::<BigEndian>(0).unwrap(); // exception_table_length
    blob.write_u16::<BigEndian>(sub_attr_count).unwrap();
    if let Some(smt) = stack_map_table {
        blob.write_u16::<BigEndian>(smt.name_idx).unwrap();
        let payload_len: u32 = 2 + smt.entries.len() as u32;
        blob.write_u32::<BigEndian>(payload_len).unwrap();
        blob.write_u16::<BigEndian>(smt.entry_count).unwrap();
        blob.write_all(&smt.entries).unwrap();
    }
    blob
}

/// Pre-built `StackMapTable` entries for a method body — the `entries`
/// slice is the serialised concatenation of every frame's bytes, and
/// `entry_count` is the number of frames it contains. `name_idx` is
/// the constant-pool Utf8 index for the literal `"StackMapTable"`
/// (caller-registered so the value-class emitter avoids depending on
/// the late-binding placeholder pipeline used by the rest of the
/// writer).
struct StackMapTable {
    name_idx: u16,
    entry_count: u16,
    entries: Vec<u8>,
}

/// Encode a `same_frame` (JVMS §4.7.4 §a) with the given
/// `offset_delta` (must be 0..=63 inclusive). For larger deltas use
/// `same_frame_extended` (frame_type = 251). Convenience builder for
/// the value-class synthetic methods where deltas stay well under 63.
fn smt_same_frame_byte(offset_delta: u8) -> u8 {
    debug_assert!(offset_delta < 64);
    offset_delta
}

/// Build a `same_locals_1_stack_item_frame` (JVMS §4.7.4 §b) with a
/// single `Integer` (verification_type_info tag=1) on the stack.
/// Used for the post-`goto` frame in `equals-impl0` where the int
/// result of either `iconst_0` or `iconst_1` is live.
fn smt_same_locals_1_stack_item_int(offset_delta: u8) -> Vec<u8> {
    debug_assert!(offset_delta < 64);
    vec![64 + offset_delta, /* ITEM_Integer */ 1]
}

/// Build all seven standard synthetic methods for a `@JvmInline value
/// class`. The returned `Vec<Vec<u8>>` is appended to the class's
/// method_blobs list. Order matches kotlinc:
///
///   0. `toString-impl(U)String`
///   1. `toString()String` — instance forwarder
///   2. `hashCode-impl(U)I`
///   3. `hashCode()I` — instance forwarder
///   4. `equals-impl(U,Object)Z`
///   5. `equals(Object)Z` — instance forwarder
///   6. `constructor-impl(U)U` — identity
///   7. `box-impl(U)V` — allocate + private-init
///   8. `unbox-impl()U` — extract the underlying
///   9. `equals-impl0(U,U)Z` — direct two-underlying equality
///
/// `class_name` is the JVM internal name of the value class (e.g.
/// `"V"` or `"com/example/UserId"`). `field_name` is the source-level
/// name of the underlying `val` parameter (`"value"`, `"x"`, `"raw"`).
/// `underlying` is its `Ty`.
pub fn emit_value_class_synthetics(
    class_name: &str,
    field_name: &str,
    underlying: &Ty,
    cp: &mut ConstantPool,
    code_attr_name_idx: u16,
) -> Vec<Vec<u8>> {
    let u_desc = underlying_descriptor(underlying);
    let u_slots = slot_width(underlying);
    // Pre-register the `StackMapTable` sub-attribute name in the
    // constant pool — only `equals-impl` and `equals-impl0` need a
    // StackMapTable (they branch on the comparison result); the other
    // synthetics are branch-free. Registering once up-front matches
    // kotlinc's CP-ordering convention (a single Utf8 entry for the
    // attribute name shared across all the methods that use it).
    let smt_name_idx = cp.utf8("StackMapTable");
    let mut blobs = Vec::new();

    blobs.push(emit_to_string_impl(
        class_name,
        field_name,
        underlying,
        &u_desc,
        u_slots,
        cp,
        code_attr_name_idx,
    ));
    blobs.push(emit_to_string_forwarder(
        class_name,
        field_name,
        &u_desc,
        u_slots,
        cp,
        code_attr_name_idx,
    ));
    blobs.push(emit_hash_code_impl(
        underlying,
        &u_desc,
        u_slots,
        cp,
        code_attr_name_idx,
    ));
    blobs.push(emit_hash_code_forwarder(
        class_name,
        field_name,
        &u_desc,
        u_slots,
        cp,
        code_attr_name_idx,
    ));
    blobs.push(emit_equals_impl(
        class_name,
        underlying,
        &u_desc,
        u_slots,
        cp,
        code_attr_name_idx,
        smt_name_idx,
    ));
    blobs.push(emit_equals_forwarder(
        class_name,
        field_name,
        &u_desc,
        u_slots,
        cp,
        code_attr_name_idx,
    ));
    blobs.push(emit_constructor_impl(
        underlying,
        &u_desc,
        u_slots,
        cp,
        code_attr_name_idx,
    ));
    blobs.push(emit_box_impl(
        class_name,
        underlying,
        &u_desc,
        u_slots,
        cp,
        code_attr_name_idx,
    ));
    blobs.push(emit_unbox_impl(
        class_name,
        field_name,
        underlying,
        &u_desc,
        u_slots,
        cp,
        code_attr_name_idx,
    ));
    blobs.push(emit_equals_impl0(
        underlying,
        &u_desc,
        u_slots,
        cp,
        code_attr_name_idx,
        smt_name_idx,
    ));
    blobs
}

/// `public static String toString-impl(<U> arg0)` — `"V(field=<arg0>)"`.
/// Mirrors kotlinc by using StringBuilder.append chain (NOT
/// invokedynamic — this is the data-class shape for value classes).
fn emit_to_string_impl(
    class_name: &str,
    field_name: &str,
    underlying: &Ty,
    u_desc: &str,
    u_slots: u16,
    cp: &mut ConstantPool,
    code_attr_name_idx: u16,
) -> Vec<u8> {
    let prefix = cp.string(&format!(
        "{}({}=",
        class_simple_name(class_name),
        field_name
    ));
    let sb_cls = cp.class("java/lang/StringBuilder");
    let sb_init = cp.methodref("java/lang/StringBuilder", "<init>", "()V");
    let append_str = cp.methodref(
        "java/lang/StringBuilder",
        "append",
        "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
    );
    let append_u_desc = format!("({u_desc})Ljava/lang/StringBuilder;");
    let append_u = cp.methodref("java/lang/StringBuilder", "append", &append_u_desc);
    let append_c = cp.methodref(
        "java/lang/StringBuilder",
        "append",
        "(C)Ljava/lang/StringBuilder;",
    );
    let to_string = cp.methodref(
        "java/lang/StringBuilder",
        "toString",
        "()Ljava/lang/String;",
    );

    let mut code = Vec::new();
    code.push(0xBB); // new java/lang/StringBuilder
    code.write_u16::<BigEndian>(sb_cls).unwrap();
    code.push(0x59); // dup
    code.push(0xB7); // invokespecial <init>()V
    code.write_u16::<BigEndian>(sb_init).unwrap();
    code.push(0x12); // ldc <prefix>
    code.push((prefix & 0xFF) as u8);
    code.push(0xB6); // invokevirtual append(String)
    code.write_u16::<BigEndian>(append_str).unwrap();
    emit_load(&mut code, underlying, 0);
    code.push(0xB6); // invokevirtual append(U)
    code.write_u16::<BigEndian>(append_u).unwrap();
    // bipush ')' = 41
    code.push(0x10);
    code.push(41);
    code.push(0xB6); // invokevirtual append(C)
    code.write_u16::<BigEndian>(append_c).unwrap();
    code.push(0xB6); // invokevirtual toString()
    code.write_u16::<BigEndian>(to_string).unwrap();
    code.push(0xB0); // areturn

    // ldc with index > 255 requires ldc_w (0x13); upgrade if needed.
    if prefix > 0xFF {
        // Replace the single-byte ldc with a 3-byte ldc_w by rebuilding
        // the relevant chunk. Find the ldc opcode by scanning back from
        // the start of the append-str invocation we emitted just after.
        // We know it's at byte offset 7 (new + dup + invokespecial = 7
        // bytes), so patch in-place to ldc_w.
        // new(3) + dup(1) + invokespecial(3) = 7 → ldc is at index 7
        code[7] = 0x13;
        // Splice the high byte before the low byte.
        code.insert(8, (prefix >> 8) as u8);
        // The low byte is already at index 9.
    }

    let name_idx = cp.utf8("toString-impl");
    let desc_idx = cp.utf8(&format!("({u_desc})Ljava/lang/String;"));
    // max_stack: StringBuilder + dup + append-with-wide-U = 3 or 4
    let max_stack: u16 = if matches!(underlying, Ty::Long | Ty::Double) {
        4
    } else {
        3
    };
    let max_locals: u16 = u_slots;
    build_method_blob(
        ACC_PUBLIC | ACC_STATIC,
        name_idx,
        desc_idx,
        max_stack,
        max_locals,
        &code,
        code_attr_name_idx,
        None,
    )
}

/// `public String toString()` — `aload_0; getfield this.<field>;
/// invokestatic toString-impl(<U>)String; areturn`.
fn emit_to_string_forwarder(
    class_name: &str,
    field_name: &str,
    u_desc: &str,
    u_slots: u16,
    cp: &mut ConstantPool,
    code_attr_name_idx: u16,
) -> Vec<u8> {
    let fr = cp.fieldref(class_name, field_name, u_desc);
    let impl_mref = cp.methodref(
        class_name,
        "toString-impl",
        &format!("({u_desc})Ljava/lang/String;"),
    );
    let mut code = Vec::new();
    code.push(0x2A); // aload_0
    code.push(0xB4); // getfield
    code.write_u16::<BigEndian>(fr).unwrap();
    code.push(0xB8); // invokestatic
    code.write_u16::<BigEndian>(impl_mref).unwrap();
    code.push(0xB0); // areturn
    let name_idx = cp.utf8("toString");
    let desc_idx = cp.utf8("()Ljava/lang/String;");
    let max_stack: u16 = u_slots; // getfield-pushed underlying
    build_method_blob(
        ACC_PUBLIC,
        name_idx,
        desc_idx,
        max_stack,
        1,
        &code,
        code_attr_name_idx,
        None,
    )
}

/// `public static int hashCode-impl(<U>)` — delegates to the boxed-type
/// `hashCode(<U>)I` static for primitives (e.g. `java/lang/Long.hashCode(J)I`),
/// or to `<obj>.hashCode()I` for reference underlyings.
fn emit_hash_code_impl(
    underlying: &Ty,
    u_desc: &str,
    u_slots: u16,
    cp: &mut ConstantPool,
    code_attr_name_idx: u16,
) -> Vec<u8> {
    let mut code = Vec::new();
    emit_load(&mut code, underlying, 0);
    match underlying {
        Ty::Bool => {
            let m = cp.methodref("java/lang/Boolean", "hashCode", "(Z)I");
            code.push(0xB8);
            code.write_u16::<BigEndian>(m).unwrap();
        }
        Ty::Byte => {
            let m = cp.methodref("java/lang/Byte", "hashCode", "(B)I");
            code.push(0xB8);
            code.write_u16::<BigEndian>(m).unwrap();
        }
        Ty::Short => {
            let m = cp.methodref("java/lang/Short", "hashCode", "(S)I");
            code.push(0xB8);
            code.write_u16::<BigEndian>(m).unwrap();
        }
        Ty::Char => {
            let m = cp.methodref("java/lang/Character", "hashCode", "(C)I");
            code.push(0xB8);
            code.write_u16::<BigEndian>(m).unwrap();
        }
        Ty::Int => {
            let m = cp.methodref("java/lang/Integer", "hashCode", "(I)I");
            code.push(0xB8);
            code.write_u16::<BigEndian>(m).unwrap();
        }
        Ty::Float => {
            let m = cp.methodref("java/lang/Float", "hashCode", "(F)I");
            code.push(0xB8);
            code.write_u16::<BigEndian>(m).unwrap();
        }
        Ty::Long => {
            let m = cp.methodref("java/lang/Long", "hashCode", "(J)I");
            code.push(0xB8);
            code.write_u16::<BigEndian>(m).unwrap();
        }
        Ty::Double => {
            let m = cp.methodref("java/lang/Double", "hashCode", "(D)I");
            code.push(0xB8);
            code.write_u16::<BigEndian>(m).unwrap();
        }
        _ => {
            // Reference underlying: virtual `hashCode()I` on Object.
            let m = cp.methodref("java/lang/Object", "hashCode", "()I");
            code.push(0xB6); // invokevirtual
            code.write_u16::<BigEndian>(m).unwrap();
        }
    }
    code.push(0xAC); // ireturn

    let name_idx = cp.utf8("hashCode-impl");
    let desc_idx = cp.utf8(&format!("({u_desc})I"));
    let max_stack: u16 = u_slots;
    let max_locals: u16 = u_slots;
    build_method_blob(
        ACC_PUBLIC | ACC_STATIC,
        name_idx,
        desc_idx,
        max_stack,
        max_locals,
        &code,
        code_attr_name_idx,
        None,
    )
}

/// `public int hashCode()` — instance forwarder to `hashCode-impl`.
fn emit_hash_code_forwarder(
    class_name: &str,
    field_name: &str,
    u_desc: &str,
    u_slots: u16,
    cp: &mut ConstantPool,
    code_attr_name_idx: u16,
) -> Vec<u8> {
    let fr = cp.fieldref(class_name, field_name, u_desc);
    let impl_mref = cp.methodref(class_name, "hashCode-impl", &format!("({u_desc})I"));
    let mut code = Vec::new();
    code.push(0x2A); // aload_0
    code.push(0xB4); // getfield
    code.write_u16::<BigEndian>(fr).unwrap();
    code.push(0xB8); // invokestatic
    code.write_u16::<BigEndian>(impl_mref).unwrap();
    code.push(0xAC); // ireturn
    let name_idx = cp.utf8("hashCode");
    let desc_idx = cp.utf8("()I");
    let max_stack: u16 = u_slots;
    build_method_blob(
        ACC_PUBLIC,
        name_idx,
        desc_idx,
        max_stack,
        1,
        &code,
        code_attr_name_idx,
        None,
    )
}

/// `public static boolean equals-impl(<U> arg0, Object other)`
/// — `if (!(other instanceof V)) return false;
///     return equals-impl0(arg0, ((V)other).unbox-impl())`.
fn emit_equals_impl(
    class_name: &str,
    underlying: &Ty,
    u_desc: &str,
    u_slots: u16,
    cp: &mut ConstantPool,
    code_attr_name_idx: u16,
    smt_name_idx: u16,
) -> Vec<u8> {
    let cls_idx = cp.class(class_name);
    let unbox_mref = cp.methodref(class_name, "unbox-impl", &format!("(){u_desc}"));
    let impl0_mref = cp.methodref(class_name, "equals-impl0", &format!("({u_desc}{u_desc})Z"));

    // Layout:
    //   slot 0: arg0 (U)        — u_slots wide
    //   slot u_slots: other     — Object
    let other_slot: u8 = u_slots as u8;

    let mut code = Vec::new();
    // Shape: aload other; instanceof V; ifne SKIP_FALSE; iconst_0;
    // ireturn; SKIP_FALSE: arg0_load; aload other; checkcast V;
    // invokevirtual unbox-impl; invokestatic equals-impl0; ireturn.
    // Branch offset is from the start of the `ifne` instruction
    // itself (JVMS §6.5.if<cond>); +5 skips the trailing 2-byte
    // `iconst_0; ireturn`.
    if other_slot <= 3 {
        code.push(0x2A + other_slot);
    } else {
        code.push(0x19);
        code.push(other_slot);
    }
    code.push(0xC1); // instanceof V
    code.write_u16::<BigEndian>(cls_idx).unwrap();
    code.push(0x9A); // ifne +5
    code.write_i16::<BigEndian>(5).unwrap();
    code.push(0x03); // iconst_0
    code.push(0xAC); // ireturn
    emit_load(&mut code, underlying, 0);
    if other_slot <= 3 {
        code.push(0x2A + other_slot);
    } else {
        code.push(0x19);
        code.push(other_slot);
    }
    code.push(0xC0); // checkcast V
    code.write_u16::<BigEndian>(cls_idx).unwrap();
    code.push(0xB6); // invokevirtual unbox-impl
    code.write_u16::<BigEndian>(unbox_mref).unwrap();
    code.push(0xB8); // invokestatic equals-impl0
    code.write_u16::<BigEndian>(impl0_mref).unwrap();
    code.push(0xAC); // ireturn

    let name_idx = cp.utf8("equals-impl");
    let desc_idx = cp.utf8(&format!("({u_desc}Ljava/lang/Object;)Z"));
    let max_stack: u16 = (u_slots * 2).max(2);
    let max_locals: u16 = u_slots + 1;
    // The single `ifne` target lands at the start of the
    // `unbox-and-delegate` path. PC layout:
    //   aload_other (1 or 2) | instanceof (3) | ifne (3) | iconst_0 (1)
    //   | ireturn (1) | branch_target
    let aload_size = if other_slot <= 3 { 1 } else { 2 };
    let branch_pc: u8 = aload_size + 3 + 3 + 1 + 1;
    // `same_frame` — locals + stack unchanged from method entry.
    let smt = StackMapTable {
        name_idx: smt_name_idx,
        entry_count: 1,
        entries: vec![smt_same_frame_byte(branch_pc)],
    };
    build_method_blob(
        ACC_PUBLIC | ACC_STATIC,
        name_idx,
        desc_idx,
        max_stack,
        max_locals,
        &code,
        code_attr_name_idx,
        Some(smt),
    )
}

/// `public boolean equals(Object other)` — forwards via field-load.
fn emit_equals_forwarder(
    class_name: &str,
    field_name: &str,
    u_desc: &str,
    u_slots: u16,
    cp: &mut ConstantPool,
    code_attr_name_idx: u16,
) -> Vec<u8> {
    let fr = cp.fieldref(class_name, field_name, u_desc);
    let impl_mref = cp.methodref(
        class_name,
        "equals-impl",
        &format!("({u_desc}Ljava/lang/Object;)Z"),
    );
    let mut code = Vec::new();
    code.push(0x2A); // aload_0
    code.push(0xB4); // getfield
    code.write_u16::<BigEndian>(fr).unwrap();
    code.push(0x2B); // aload_1
    code.push(0xB8); // invokestatic
    code.write_u16::<BigEndian>(impl_mref).unwrap();
    code.push(0xAC); // ireturn
    let name_idx = cp.utf8("equals");
    let desc_idx = cp.utf8("(Ljava/lang/Object;)Z");
    let max_stack: u16 = u_slots + 1;
    let max_locals: u16 = 2;
    build_method_blob(
        ACC_PUBLIC,
        name_idx,
        desc_idx,
        max_stack,
        max_locals,
        &code,
        code_attr_name_idx,
        None,
    )
}

/// `public static <U> constructor-impl(<U> arg0)` — identity. A future
/// phase will splice user `init { … }` validation here.
fn emit_constructor_impl(
    underlying: &Ty,
    u_desc: &str,
    u_slots: u16,
    cp: &mut ConstantPool,
    code_attr_name_idx: u16,
) -> Vec<u8> {
    let mut code = Vec::new();
    emit_load(&mut code, underlying, 0);
    code.push(return_op(underlying));
    let name_idx = cp.utf8("constructor-impl");
    let desc_idx = cp.utf8(&format!("({u_desc}){u_desc}"));
    build_method_blob(
        ACC_PUBLIC | ACC_STATIC,
        name_idx,
        desc_idx,
        u_slots,
        u_slots,
        &code,
        code_attr_name_idx,
        None,
    )
}

/// `public static synthetic V box-impl(<U> arg0)` — `new V; dup; <load
/// arg0>; invokespecial V.<init>(<U>)V; areturn`.
fn emit_box_impl(
    class_name: &str,
    underlying: &Ty,
    u_desc: &str,
    u_slots: u16,
    cp: &mut ConstantPool,
    code_attr_name_idx: u16,
) -> Vec<u8> {
    let cls_idx = cp.class(class_name);
    let init_mref = cp.methodref(class_name, "<init>", &format!("({u_desc})V"));

    let mut code = Vec::new();
    code.push(0xBB); // new V
    code.write_u16::<BigEndian>(cls_idx).unwrap();
    code.push(0x59); // dup
    emit_load(&mut code, underlying, 0);
    code.push(0xB7); // invokespecial V.<init>(U)V
    code.write_u16::<BigEndian>(init_mref).unwrap();
    code.push(0xB0); // areturn

    let name_idx = cp.utf8("box-impl");
    let desc_idx = cp.utf8(&format!("({u_desc})L{class_name};"));
    let max_stack: u16 = 2 + u_slots;
    build_method_blob(
        ACC_PUBLIC | ACC_STATIC | ACC_FINAL | ACC_SYNTHETIC,
        name_idx,
        desc_idx,
        max_stack,
        u_slots,
        &code,
        code_attr_name_idx,
        None,
    )
}

/// `public final synthetic <U> unbox-impl()` — `aload_0; getfield
/// this.<field>; <Xreturn>`. Mirrors the standard property-getter
/// shape with the synthetic-flag bit set.
fn emit_unbox_impl(
    class_name: &str,
    field_name: &str,
    underlying: &Ty,
    u_desc: &str,
    u_slots: u16,
    cp: &mut ConstantPool,
    code_attr_name_idx: u16,
) -> Vec<u8> {
    let fr = cp.fieldref(class_name, field_name, u_desc);
    let mut code = Vec::new();
    code.push(0x2A); // aload_0
    code.push(0xB4); // getfield
    code.write_u16::<BigEndian>(fr).unwrap();
    code.push(return_op(underlying));
    let name_idx = cp.utf8("unbox-impl");
    let desc_idx = cp.utf8(&format!("(){u_desc}"));
    build_method_blob(
        ACC_PUBLIC | ACC_FINAL | ACC_SYNTHETIC,
        name_idx,
        desc_idx,
        u_slots,
        1,
        &code,
        code_attr_name_idx,
        None,
    )
}

/// `public static boolean equals-impl0(<U> p1, <U> p2)` — direct
/// two-underlying equality. The shape is `<cmp>; ifne +7; iconst_1;
/// goto +4; iconst_0; ireturn`. For reference underlyings, uses
/// `if_acmp{eq,ne}`; for primitives uses the appropriate `cmp`
/// instruction plus `if{eq,ne}`.
fn emit_equals_impl0(
    underlying: &Ty,
    u_desc: &str,
    u_slots: u16,
    cp: &mut ConstantPool,
    code_attr_name_idx: u16,
    smt_name_idx: u16,
) -> Vec<u8> {
    let mut code = Vec::new();
    emit_load(&mut code, underlying, 0);
    emit_load(&mut code, underlying, u_slots as u8);

    // Push 0/1 based on equality. Use type-appropriate cmp.
    match underlying {
        Ty::Long => {
            code.push(0x94); // lcmp -> int (0 if equal)
                             // ifne +7 (jump past iconst_1 + goto)
            code.push(0x9A);
            code.write_i16::<BigEndian>(7).unwrap();
            code.push(0x04); // iconst_1
            code.push(0xA7); // goto +4
            code.write_i16::<BigEndian>(4).unwrap();
            code.push(0x03); // iconst_0
        }
        Ty::Float => {
            code.push(0x96); // fcmpl
            code.push(0x9A);
            code.write_i16::<BigEndian>(7).unwrap();
            code.push(0x04);
            code.push(0xA7);
            code.write_i16::<BigEndian>(4).unwrap();
            code.push(0x03);
        }
        Ty::Double => {
            code.push(0x98); // dcmpl
            code.push(0x9A);
            code.write_i16::<BigEndian>(7).unwrap();
            code.push(0x04);
            code.push(0xA7);
            code.write_i16::<BigEndian>(4).unwrap();
            code.push(0x03);
        }
        Ty::Bool | Ty::Byte | Ty::Short | Ty::Char | Ty::Int => {
            // if_icmpne +7
            code.push(0xA0);
            code.write_i16::<BigEndian>(7).unwrap();
            code.push(0x04); // iconst_1
            code.push(0xA7); // goto +4
            code.write_i16::<BigEndian>(4).unwrap();
            code.push(0x03); // iconst_0
        }
        _ => {
            // Reference equality (instance equals would loop back into
            // this method via the wrapper). For value-class semantics
            // kotlinc uses `<U>.equals(other)` — delegate to the
            // underlying type's equals(Object)Z for String/etc.
            let eq = cp.methodref("java/lang/Object", "equals", "(Ljava/lang/Object;)Z");
            code.push(0xB6); // invokevirtual equals
            code.write_u16::<BigEndian>(eq).unwrap();
        }
    }
    code.push(0xAC); // ireturn

    let name_idx = cp.utf8("equals-impl0");
    let desc_idx = cp.utf8(&format!("({u_desc}{u_desc})Z"));
    let max_stack: u16 = (u_slots * 2).max(2);
    let max_locals: u16 = u_slots * 2;
    // PC layout per underlying type:
    //   load1 (1) | load2 (1) | [Xcmp (1 if wide+real-cmp)] | branch (3)
    //   | iconst_1 (1) | goto (3) | iconst_0 (1) | ireturn (1)
    // Wide-cmp types (Long/Float/Double) insert a 1-byte XcmpY before
    // the conditional branch; Int/Bool/Byte/Short/Char skip that and
    // branch via if_icmpne directly.
    let cmp_byte = matches!(underlying, Ty::Long | Ty::Float | Ty::Double) as u8;
    let needs_smt = !matches!(
        underlying,
        Ty::String | Ty::Class(_) | Ty::Any | Ty::Nullable(_)
    );
    let smt = if needs_smt {
        // Branch target 1 (iconst_0): right after `load1; load2;
        // [Xcmp]; branch(3); iconst_1(1); goto(3)` = 9 + cmp_byte.
        let pc_iconst_0 = 9 + cmp_byte;
        // Branch target 2 (ireturn): one byte after iconst_0.
        let pc_ireturn = pc_iconst_0 + 1;
        let mut entries: Vec<u8> = Vec::with_capacity(3);
        // Frame 1: same_frame at pc_iconst_0 (offset_delta = pc_iconst_0).
        entries.push(smt_same_frame_byte(pc_iconst_0));
        // Frame 2: same_locals_1_stack_item_frame (Integer) at pc_ireturn,
        // offset_delta = pc_ireturn - pc_iconst_0 - 1.
        entries.extend_from_slice(&smt_same_locals_1_stack_item_int(
            pc_ireturn - pc_iconst_0 - 1,
        ));
        Some(StackMapTable {
            name_idx: smt_name_idx,
            entry_count: 2,
            entries,
        })
    } else {
        // Reference-typed equals: no branches in the body (single
        // `invokevirtual equals; ireturn`); no StackMapTable needed.
        None
    };
    build_method_blob(
        ACC_PUBLIC | ACC_STATIC | ACC_FINAL,
        name_idx,
        desc_idx,
        max_stack,
        max_locals,
        &code,
        code_attr_name_idx,
        smt,
    )
}

/// Strip the package prefix from a JVM-internal name. `com/foo/Bar`
/// → `Bar`, `Bar` → `Bar`. Used by the `toString-impl` builder to
/// reproduce kotlinc's `<SimpleName>(<field>=<value>)` rendering.
fn class_simple_name(jvm_internal: &str) -> &str {
    jvm_internal.rsplit('/').next().unwrap_or(jvm_internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── KEEP-104 mangling spec (cross-checked against kotlinc) ──────

    fn mp_value(fq: &str) -> MangleParam {
        MangleParam {
            fq_name: fq.to_string(),
            is_value: true,
            is_nullable: false,
        }
    }

    fn mp_value_nullable(fq: &str) -> MangleParam {
        MangleParam {
            fq_name: fq.to_string(),
            is_value: true,
            is_nullable: true,
        }
    }

    fn mp_plain() -> MangleParam {
        MangleParam {
            fq_name: String::new(),
            is_value: false,
            is_nullable: false,
        }
    }

    #[test]
    fn mangle_single_value_param_matches_kotlinc() {
        // `fun combine(other: V): Long` on `value class V(val x: Long)`
        // — single value-class param `V`. kotlinc emits
        // `combine-Wv3fJ18`. The signature input is `"LV;"`.
        let m = mangle_method_name("combine", &[mp_value("V")]).expect("mangled");
        assert_eq!(m, "combine-Wv3fJ18");
    }

    #[test]
    fn mangle_two_value_params_matches_kotlinc() {
        // `fun mix(a: V, b: V): Long` → input `"LV;LV;"` → `ddrBm3U`.
        let m = mangle_method_name("mix", &[mp_value("V"), mp_value("V")]).expect("mangled");
        assert_eq!(m, "mix-ddrBm3U");
    }

    #[test]
    fn mangle_three_value_params_matches_kotlinc() {
        // `fun mix(a: V, b: V, c: V)` → `"LV;LV;LV;"`.
        let expected_input = "LV;LV;LV;";
        let mut hasher = Md5::new();
        hasher.update(expected_input.as_bytes());
        let expected = base64url_no_pad(&hasher.finalize()[..5]);
        let m = mangle_method_name("mix", &[mp_value("V"), mp_value("V"), mp_value("V")])
            .expect("mangled");
        assert_eq!(m, format!("mix-{expected}"));
    }

    #[test]
    fn mangle_mixed_value_and_plain_matches_kotlinc() {
        // `fun mixed(s: String, a: V, n: Int): Long` → input `"_LV;_"`
        // → `loMGqfo`.
        let m =
            mangle_method_name("mixed", &[mp_plain(), mp_value("V"), mp_plain()]).expect("mangled");
        assert_eq!(m, "mixed-loMGqfo");
    }

    #[test]
    fn mangle_returns_none_for_no_value_params() {
        // Methods with no value-class parameters do NOT get the hash
        // suffix. `fun double(): Long` and `fun describe(): String`
        // both produce the plain `<name>-impl` shape via the
        // `static_impl_name` helper (NOT through this hash function).
        assert!(mangle_method_name("double", &[]).is_none());
        assert!(mangle_method_name("named", &[mp_plain(), mp_plain()]).is_none());
    }

    #[test]
    fn mangle_nullable_value_param_appends_question_mark() {
        // `fun f(v: V?): Unit` → input `"LV?;"`. Distinct from `LV;`,
        // so a different hash.
        let m = mangle_method_name("f", &[mp_value_nullable("V")]).expect("mangled");
        let mut hasher = Md5::new();
        hasher.update(b"LV?;");
        let expected = base64url_no_pad(&hasher.finalize()[..5]);
        assert_eq!(m, format!("f-{expected}"));
        // Distinct from non-nullable form.
        let m2 = mangle_method_name("f", &[mp_value("V")]).expect("mangled");
        assert_ne!(m, m2);
    }

    #[test]
    fn mangle_packaged_value_param_uses_dotted_fq_name() {
        // The mangling uses the kotlinc-shape `fqName` which is the
        // dotted Kotlin FQ-name. Callers convert from JVM-internal
        // (slashes) to dotted before constructing `MangleParam`.
        let m = mangle_method_name("f", &[mp_value("com.example.UserId")]).expect("mangled");
        let mut hasher = Md5::new();
        hasher.update(b"Lcom.example.UserId;");
        let expected = base64url_no_pad(&hasher.finalize()[..5]);
        assert_eq!(m, format!("f-{expected}"));
    }

    // ── Base64URL encoder ────────────────────────────────────────────

    #[test]
    fn base64url_no_pad_5_bytes_matches_java_encoder() {
        // The single call site we care about. Sentinel input "12345".
        let s = base64url_no_pad(b"12345");
        assert_eq!(s, "MTIzNDU"); // 7 chars, no padding.
    }

    #[test]
    fn base64url_no_pad_handles_url_safe_substitutions() {
        // 5 bytes whose base64 encoding includes both `-` and `_` —
        // these would be `+` and `/` in standard base64.
        // 0xFF 0xFF 0xFF 0xFF 0xFF → "____" + remainder
        let s = base64url_no_pad(&[0xFF; 5]);
        // Standard base64 of 5 0xFFs = "//////8" → URL-safe: "______8"
        assert_eq!(s, "______8");
    }

    // ── Helpers ──────────────────────────────────────────────────────

    #[test]
    fn primitive_descriptor_char_known_types() {
        assert_eq!(primitive_descriptor_char(&Ty::Long), Some('J'));
        assert_eq!(primitive_descriptor_char(&Ty::Int), Some('I'));
        assert_eq!(primitive_descriptor_char(&Ty::Float), Some('F'));
        assert_eq!(primitive_descriptor_char(&Ty::Double), Some('D'));
        assert_eq!(primitive_descriptor_char(&Ty::String), None);
        assert_eq!(primitive_descriptor_char(&Ty::Class("V".to_string())), None);
    }

    #[test]
    fn underlying_descriptor_primitives_and_classes() {
        assert_eq!(underlying_descriptor(&Ty::Long), "J");
        assert_eq!(underlying_descriptor(&Ty::String), "Ljava/lang/String;");
        assert_eq!(
            underlying_descriptor(&Ty::Class("com/foo/Bar".to_string())),
            "Lcom/foo/Bar;"
        );
    }

    #[test]
    fn class_simple_name_strips_package() {
        assert_eq!(class_simple_name("com/foo/Bar"), "Bar");
        assert_eq!(class_simple_name("Bar"), "Bar");
        assert_eq!(class_simple_name(""), "");
    }
}
