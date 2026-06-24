//! Backend-neutral mid-level IR.
//!
//! MIR is the **narrow waist** between the front-end (lex/parse/resolve/
//! typeck) and the backends (`skotch-backend-jvm`, `-dex`, `-llvm`,
//! `-wasm`). The core shape is: a flat list of basic blocks per
//! function, three-address-code-style assignments into virtual locals,
//! a tiny `Rvalue` enum, and a `Terminator` per block.
//!
//! ## What we model
//!
//! - Constant loads (string, int, bool, unit)
//! - Local reads
//! - Calls — either to other top-level functions (`Static`) or to
//!   the hard-coded `Println` intrinsic
//! - Integer arithmetic (`Add`/`Sub`/`Mul`/`Div`/`Mod`)
//! - `Return` and `ReturnValue` terminators
//!
//! ## Not yet supported
//!
//! - Branches (`if`/`when`) — needs `Terminator::Branch` plus `Switch`,
//!   plus JVM `StackMapTable` support in the backend.
//! - String templates — needs a `Concat` intrinsic backed by
//!   `StringBuilder` on JVM.
//! - Top-level vals lowered to static fields with `<clinit>`.
//! - Function parameters with non-`String` types beyond a single arg.
//!
//! These are tracked in `tests/fixtures/inputs/` as `status = "stub"`
//! fixtures and will graduate to "supported" as the corresponding
//! backend support lands.

use serde::{Deserialize, Serialize};
use skotch_types::Ty;

pub mod dump;
pub mod validate;

fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}

/// Default `true` for serde fields whose absence in older JSON should
/// be treated as "yes" (e.g. [`MirClass::has_explicit_primary_ctor`]).
fn skotch_default_true() -> bool {
    true
}

/// Skip serializing a `bool` field when it's the default `true`.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn skotch_is_true(v: &bool) -> bool {
    *v
}

/// Side-channel metadata for cross-file top-level functions. Carries
/// declaration-level facts the call site needs but `cross_file_fns`'s
/// `(owner_class, descriptor, return_ty)` tuple doesn't hold:
/// `inline`, extension receiver type, per-param default markers, and
/// per-param vararg markers. Populated by mir-lower from
/// [`skotch_resolve::ExternalFunDecl`] during multi-file lowering.
#[derive(Clone, Debug, Default)]
pub struct CrossFileFnExtras {
    pub is_inline: bool,
    pub receiver_ty: Option<Ty>,
    pub has_default: Vec<bool>,
    pub is_vararg: Vec<bool>,
    pub annotations: Vec<String>,
}

// ── Annotations ─────────────────────────────────────────────────────────────

/// An annotation carried through MIR for JVM emission.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirAnnotation {
    /// Fully-qualified JVM type descriptor, e.g. "Lkotlin/jvm/JvmStatic;".
    pub descriptor: String,
    /// Element-value pairs for annotation arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<MirAnnotationArg>,
    /// Retention policy. Only RUNTIME annotations are emitted to .class.
    #[serde(default)]
    pub retention: AnnotationRetention,
}

/// A single annotation argument (element-value pair).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirAnnotationArg {
    pub name: String,
    pub value: MirAnnotationValue,
}

/// Annotation argument value types matching JVM element_value spec.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MirAnnotationValue {
    String(String),
    Int(i32),
    Bool(bool),
    /// Class reference: "Ljava/lang/String;"
    Class(String),
    /// Enum constant: (type_descriptor, constant_name)
    Enum(String, String),
    /// Array of values.
    Array(Vec<MirAnnotationValue>),
}

/// Annotation retention policy.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnnotationRetention {
    /// Discarded after compilation. Not emitted to .class.
    Source,
    /// Stored in .class but not available at runtime via reflection.
    Binary,
    /// Stored in .class and available at runtime via reflection.
    #[default]
    Runtime,
}

/// Identifier for a virtual local inside a function. Locals are dense:
/// 0..N for parameters and `val`/`var` declarations.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LocalId(pub u32);

/// Identifier for a string in [`MirModule::strings`]. The pool is
/// insertion-order stable so backends can iterate it deterministically.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StringId(pub u32);

/// Identifier for a top-level function in [`MirModule::functions`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FuncId(pub u32);

/// Compile-time-known constants. The string variant references the
/// module-level pool so backends can dedupe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MirConst {
    Unit,
    Bool(bool),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Null,
    String(StringId),
}

/// Right-hand side of an assignment statement. Three-address-code style.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Rvalue {
    Const(MirConst),
    Local(LocalId),
    BinOp {
        op: BinOp,
        lhs: LocalId,
        rhs: LocalId,
    },
    /// Read a static field: `ClassName.FIELD`.
    /// On JVM this emits a `getstatic` instruction. The descriptor is the
    /// JVM field type descriptor (e.g. `"Lkotlinx/coroutines/GlobalScope;"`).
    GetStaticField {
        class_name: std::string::String,
        field_name: std::string::String,
        descriptor: std::string::String,
    },
    /// Create a new instance of a class (uninitialized — followed by Constructor call).
    NewInstance(std::string::String),
    /// Read an instance field: `receiver.field_name`.
    GetField {
        receiver: LocalId,
        class_name: std::string::String,
        field_name: std::string::String,
    },
    /// Write an instance field: `receiver.field_name = value`.
    PutField {
        receiver: LocalId,
        class_name: std::string::String,
        field_name: std::string::String,
        value: LocalId,
    },
    /// Write a static field: `ClassName.FIELD = value`.
    /// On JVM this emits `getstatic`-less `putstatic`. Used by the synthetic
    /// `<clinit>` to initialize non-const top-level vals with non-literal
    /// initializers. The `dest` of the enclosing `MStmt::Assign` is unused
    /// (this is a side effect; semantically returns Unit).
    PutStaticField {
        class_name: std::string::String,
        field_name: std::string::String,
        descriptor: std::string::String,
        value: LocalId,
    },
    Call {
        kind: CallKind,
        args: Vec<LocalId>,
    },
    /// Runtime type check: `obj is Type` → Bool.
    /// `type_descriptor` is the JVM internal name (e.g. "java/lang/String").
    InstanceOf {
        obj: LocalId,
        type_descriptor: std::string::String,
    },
    /// Create a new primitive `int[]` of the given size.
    /// Result type is `Ty::IntArray`.
    NewIntArray(LocalId),
    /// Load an element from an `IntArray`: `array[index]`.
    /// Result type is `Ty::Int`.
    ArrayLoad {
        array: LocalId,
        index: LocalId,
    },
    /// Store an element into an `IntArray`: `array[index] = value`.
    /// Result type is `Ty::Unit` (the dest local is unused).
    ArrayStore {
        array: LocalId,
        index: LocalId,
        value: LocalId,
    },
    /// Get the length of an array: `array.size`.
    /// Result type is `Ty::Int`.
    ArrayLength(LocalId),
    /// Create a new `Object[]` array of the given size.
    /// Result type is `Ty::Any` (reference to `[Ljava/lang/Object;`).
    NewObjectArray(LocalId),
    /// Create a new typed object array (`T[]`) of the given size.
    /// The `element_class` is the JVM internal name (e.g. `"kotlin/Pair"`).
    /// Result type is `Ty::Any` (reference to `[LelementClass;`).
    NewTypedObjectArray {
        size: LocalId,
        element_class: String,
    },
    /// Store an element into an `Object[]`: `array[index] = value`.
    /// Result type is `Ty::Unit` (the dest local is unused).
    ObjectArrayStore {
        array: LocalId,
        index: LocalId,
        value: LocalId,
    },
    /// Downcast a reference: `(ClassName) obj`.
    /// On JVM this emits a `checkcast` instruction. The result local
    /// should be typed as `Class(target_class)`.
    CheckCast {
        obj: LocalId,
        target_class: String,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinOp {
    AddI,
    SubI,
    MulI,
    DivI,
    ModI,
    AddL,
    SubL,
    MulL,
    DivL,
    ModL,
    AddD,
    SubD,
    MulD,
    DivD,
    ModD,
    AddF,
    SubF,
    MulF,
    DivF,
    ModF,
    /// String concatenation: lhs (String) + rhs (any → toString).
    ConcatStr,
    /// Integer comparisons — produce a `Bool` local.
    CmpEq,
    CmpNe,
    CmpLt,
    CmpGt,
    CmpLe,
    CmpGe,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CallKind {
    /// Static call to another top-level function in the same module.
    Static(FuncId),
    /// Hard-coded `println` intrinsic.
    Println,
    /// Hard-coded `print` intrinsic (no trailing newline).
    Print,
    /// Hard-coded `println` of a string template, fused with the
    /// concatenation step. The args are the *parts* of the template
    /// in source order: literal text chunks (typed `String`) and
    /// runtime values (any type). Each backend implements this
    /// differently:
    ///
    /// - JVM/DEX build a `StringBuilder`, append each part, then
    ///   call `println(String)` on the result.
    /// - LLVM emits a single `printf` call with a format string
    ///   computed from the constant text parts (`%s`/`%d` for the
    ///   runtime parts).
    ///
    /// We fuse the concat into the println at MIR-lower time so the
    /// LLVM backend never has to materialize an intermediate
    /// concatenated `String` value (which would require either
    /// `malloc` or a runtime helper). The fixtures we currently
    /// support all use string templates as the immediate argument
    /// of `println`, so this fused form covers everything we need.
    PrintlnConcat,
    /// Like `PrintlnConcat` but without the trailing newline — fuses
    /// a string-template concat with `print(String)` instead of
    /// `println(String)`. Args are the template parts in source order.
    PrintConcat,
    /// Static method call on a Java/Kotlin class: `System.currentTimeMillis()`.
    StaticJava {
        class_name: std::string::String,
        method_name: std::string::String,
        /// JVM method descriptor, e.g. "()J" for currentTimeMillis.
        descriptor: std::string::String,
    },
    /// Virtual method call with an explicit descriptor (for JVM built-in
    /// types where the descriptor can't be inferred from MIR types alone,
    /// e.g. `String.contains(CharSequence)` where arg is typed as String).
    VirtualJava {
        class_name: std::string::String,
        method_name: std::string::String,
        descriptor: std::string::String,
    },
    /// Constructor call: `new ClassName(args)`.
    Constructor(std::string::String),
    /// Constructor call on an external Java/Kotlin class with explicit
    /// descriptor (the class is not in the current MIR module).
    ConstructorJava {
        class_name: std::string::String,
        descriptor: std::string::String,
    },
    /// Virtual method call on an instance: `receiver.method(args)`.
    Virtual {
        class_name: std::string::String,
        method_name: std::string::String,
    },
    /// Super method call: `super.method(args)` — dispatches to parent class
    /// via `invokespecial` instead of `invokevirtual`.
    Super {
        class_name: std::string::String,
        method_name: std::string::String,
    },
    /// JDK 9+ string concatenation via `invokedynamic
    /// makeConcatWithConstants`. `recipe` encodes the constant
    /// segments interleaved with `` placeholders for the dynamic
    /// args (one per element of the call's `args` list). `descriptor`
    /// is the call-site signature (e.g.
    /// `(Ljava/lang/String;I)Ljava/lang/String;`).
    MakeConcatWithConstants {
        recipe: std::string::String,
        descriptor: std::string::String,
    },
    /// Create a Kotlin Function instance via JDK's LambdaMetafactory.
    /// Emits `invokedynamic invoke(<captures>)Lkotlin/jvm/functions/FunctionN;`
    /// referring to a static helper method that holds the lambda body.
    /// The call's `args` list carries the captured values (in order).
    LambdaMetafactory {
        /// FunctionN arity (number of lambda params, 0..=22).
        arity: u8,
        /// Static helper method name on `impl_class`.
        method_name: std::string::String,
        /// Specialized impl method descriptor including captures and
        /// user params, e.g. `"(II)I"` for `(capture_int, user_int) → int`.
        specialized_descriptor: std::string::String,
        /// Instantiated SAM descriptor — user params only, ALL types
        /// must be reference (Object subtype). E.g. `"(Integer)Integer"`.
        instantiated_descriptor: std::string::String,
        /// Class containing the static helper method (typically the
        /// wrapper class, e.g. "InputKt").
        impl_class: std::string::String,
    },
    /// Invoke a Kotlin Function instance via `invokeinterface
    /// FunctionN.invoke(...)`. The first arg is the Function instance
    /// (receiver), remaining args are the lambda's invoke args. Each
    /// invoke arg is treated as an Object on the wire; the call result
    /// is always Object.
    FunctionInvoke { arity: u8 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Stmt {
    Assign { dest: LocalId, value: Rvalue },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Terminator {
    Return,
    ReturnValue(LocalId),
    /// Conditional branch. `cond` must be a `Bool`-typed local.
    /// Jumps to `then_block` if true, `else_block` if false.
    /// Block indices are positions in `MirFunction::blocks`.
    Branch {
        cond: LocalId,
        then_block: u32,
        else_block: u32,
    },
    /// Unconditional jump to another block.
    Goto(u32),
    /// Throw an exception. The local must be a reference-typed exception object.
    Throw(LocalId),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BasicBlock {
    pub stmts: Vec<Stmt>,
    pub terminator: Terminator,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirFunction {
    pub id: FuncId,
    /// Source-language name (e.g. `main`). The backend wraps this in
    /// the file's wrapper class (`HelloKt`).
    pub name: String,
    pub params: Vec<LocalId>,
    /// Type of every local slot, indexed by [`LocalId`].
    pub locals: Vec<Ty>,
    pub blocks: Vec<BasicBlock>,
    pub return_ty: Ty,
    /// Number of required parameters (those without defaults).
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub required_params: usize,
    /// Parameter names (for named argument resolution at call sites).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_names: Vec<String>,
    /// For parameters with extension function type (e.g.,
    /// `block: StringBuilder.() -> Unit`), maps param index to
    /// the receiver class name. Used at call sites to set up
    /// lambda-with-receiver dispatch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_receiver_types: Vec<(usize, String)>,
    /// Default values for optional parameters, indexed by param position.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_defaults: Vec<Option<MirConst>>,
    /// True for abstract methods (no body, no Code attribute on JVM).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_abstract: bool,
    /// Index of the vararg parameter (if any). At the call site, all
    /// arguments from this position onward are packed into an IntArray.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vararg_index: Option<usize>,
    /// Exception handlers (try-catch) for this function.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exception_handlers: Vec<ExceptionHandler>,
    /// True for `suspend fun` declarations. The MIR lowerer has
    /// already applied the first half of the CPS transform: a
    /// trailing `$completion: Continuation` parameter has been
    /// injected and `return_ty` has been rewritten to `Ty::Any`
    /// (mapping to `Ljava/lang/Object;` on JVM). The full state-
    /// machine transform for multiple suspension points is still
    /// future work — see milestones.yaml v0.9.0.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_suspend: bool,
    /// True for `inline fun` declarations. The function body should be
    /// inlined at call sites rather than emitted as a separate method.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_inline: bool,
    /// True for `tailrec fun` declarations. The JVM backend rewrites
    /// self-recursive tail calls (`return f(...)`) into a goto-to-entry
    /// loop. Without this, `tailrec fun sumTo(n: Int, acc: Long)`-style
    /// deep recursion blows the JVM stack at runtime even though kotlinc
    /// compiles it to a constant-stack loop.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_tailrec: bool,
    /// True when the source declaration had type parameters
    /// (`fun <T> identity(x: T): T`). Generic type variables erase to
    /// `Ty::Any` on the JVM but kotlinc skips
    /// `Intrinsics.checkNotNullParameter` for them — we mirror that
    /// behavior in the JVM backend.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_type_params: bool,
    /// The Kotlin-source-level declared return type of a
    /// suspend function, captured before the CPS transform rewrites
    /// `return_ty` to `Ty::Any`. Call sites need this to emit the
    /// right `checkcast` on resume (e.g. `checkcast String` after a
    /// `suspend fun greet(String): String` invoke). `None` for non-
    /// suspend functions; `Some(Ty::Unit)` for suspend funs that
    /// don't return a meaningful value (no checkcast required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspend_original_return_ty: Option<Ty>,
    /// If present, this function's body is generated as a coroutine
    /// state machine. The JVM backend bypasses the normal block-walking
    /// codegen and emits the canonical dispatcher + tableswitch pattern
    /// kotlinc produces for suspend functions. Only set when the function
    /// has **exactly one** suspension point; zero-suspension bodies
    /// keep the plain signature-rewrite shape and multi-suspension bodies
    /// are rejected at lowering time until multi-point support lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspend_state_machine: Option<SuspendStateMachine>,
    // NOTE: `has_unresolved` is detected at emit time by `has_null_stubs()`
    // in the JVM backend, so we don't need a field here.
    /// Annotations on this function (emitted as RuntimeVisibleAnnotations).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<MirAnnotation>,
    /// Locals corresponding to source-level `val`/`var` declarations.
    /// These are "named" locals that should be preserved in the bytecode
    /// (matching kotlinc's behavior of keeping named locals in slots even
    /// when they could be optimized away). Anonymous compiler-generated
    /// temporaries are NOT in this list and may be eliminated by peephole
    /// optimizations like the swap pattern.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named_locals: Vec<LocalId>,
    /// True for `private` source-level functions. The JVM backend emits
    /// these with `ACC_PRIVATE` instead of `ACC_PUBLIC`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_private: bool,
    /// True for methods on a class that should be emitted with
    /// `ACC_STATIC` rather than as instance methods. The JVM backend
    /// switches the method header's [`MethodKind`] accordingly and
    /// skips reserving slot 0 for `this`. Currently set by mir-lower
    /// when synthesizing the static delegate that backs a companion
    /// `@JvmStatic` method on the outer class. Defaults to `false` so
    /// existing fixtures are unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_static: bool,
    /// Default-arg call sites: maps `(block_idx, stmt_idx)` → bit mask
    /// of which positional args came from `param_defaults`. The JVM
    /// backend uses this to route the call through the corresponding
    /// `name$default(args, mask, marker)` synthetic instead of the
    /// original method, matching kotlinc's emission.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_call_masks: Vec<(u32, u32, u32)>,
    /// kotlinc emits a leading `nop` (offset 0) for functions whose body
    /// is a single expression/statement of these shapes: a `when` without
    /// a subject, an `if`/`else if`/`else` chain, or a try-catch. The nop
    /// gives the function's `LineNumberTable` a slot that points at the
    /// function-declaration line distinct from the body's first
    /// instruction line. Set during MIR lowering by inspecting the AST.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_leading_nop: bool,
    /// Per-local generic-arg hint: maps a local's index to the type
    /// arguments of its declared type. E.g. for `val xs: List<Measurable>`
    /// at local 3, `local_generic_args[3] = [Ty::Class("Measurable")]`.
    /// Used at index access and method-call sites to recover element
    /// types that would otherwise erase to `Ty::Any` on the JVM. Sparse
    /// (only populated for locals where the source TypeRef had type
    /// args), so it doesn't bloat memory on uses without generics.
    #[serde(default, skip_serializing_if = "rustc_hash::FxHashMap::is_empty")]
    pub local_generic_args: rustc_hash::FxHashMap<u32, Vec<skotch_types::Ty>>,
}

/// Metadata describing the shape of a coroutine state machine, either
/// single- or multi-suspension-point. Carried on a [`MirFunction`] so
/// the JVM backend can emit the canonical kotlinc-style bytecode
/// without having to rediscover the structure.
///
/// ## Single-suspension shape
///
/// [`SuspendStateMachine::sites`] is empty. The `suspend_call_class` /
/// `suspend_call_method` / `resume_return_text` fields describe the
/// one callee and the literal-string tail. The backend emits the
/// original hand-rolled shape.
///
/// ## Multi-suspension shape
///
/// [`SuspendStateMachine::sites`] is non-empty — one entry per suspend
/// call in source order — and [`SuspendStateMachine::spill_layout`]
/// records the synthetic `I$n` / `L$n` / etc fields the continuation
/// class needs for locals live across any suspend. The backend
/// splits the function body into segments on the call sites and
/// emits the dispatcher + tableswitch + per-case body by walking the
/// MIR between sites.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuspendStateMachine {
    /// JVM internal name of the synthetic `ContinuationImpl` subclass
    /// the backend generates alongside this function, e.g.
    /// `"InputKt$run$1"`.
    pub continuation_class: String,
    /// JVM internal name of the class that owns the outer (state-
    /// machine-bearing) suspend function, e.g. `"InputKt"`. The
    /// synthetic `invokeSuspend` method re-invokes this function
    /// when the coroutine resumes, so the `invokeSuspend` body
    /// needs the owning class's method-ref.
    pub outer_class: String,
    /// Source-level name of the outer suspend function (e.g.
    /// `"run"`). Paired with [`SuspendStateMachine::outer_class`]
    /// to drive the `invokestatic` inside `invokeSuspend`.
    pub outer_method: String,
    /// Types of the outer function's user parameters (everything
    /// except the trailing `$completion`). Empty for zero-arg suspend
    /// functions. The `invokeSuspend` method pushes dummy values of
    /// these types before the Continuation when calling back into
    /// the outer function.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outer_user_param_tys: Vec<Ty>,
    /// JVM internal name of the class that owns the callee suspend
    /// function (single-callee shape). For same-file
    /// suspends this is the wrapper class, e.g. `"InputKt"`.
    /// Unused when [`SuspendStateMachine::sites`] is populated —
    /// multi-suspend bodies record per-site callees there.
    pub suspend_call_class: String,
    /// Source-level name of the callee suspend function, e.g.
    /// `"yield_"` (single-callee shape). The descriptor is
    /// always `(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;`.
    pub suspend_call_method: String,
    /// Pre-resolved constant the function returns once the
    /// suspended callee resumes. For `MirConst::String` the MIR
    /// lowerer resolves the pool index to the literal text so the
    /// JVM backend can intern the string in its own constant pool
    /// without having to thread the module reference through.
    /// Only meaningful for the single-suspension shape;
    /// multi-suspension bodies emit the real return expression.
    pub resume_return_text: String,
    /// One entry per suspend call in the outer function,
    /// in source order. Empty for the single-suspension
    /// shape (which uses the `suspend_call_*` + `resume_return_text`
    /// fields instead).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<SuspendCallSite>,
    /// Spill slots the continuation class needs. Ordered
    /// to match the order backends emit fields. Empty when no local
    /// crosses a suspend boundary (including the single-suspension shape).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spill_layout: Vec<SpillSlot>,
    /// True when the outer function is an instance method.
    /// The continuation's `invokeSuspend` must use `invokevirtual`
    /// instead of `invokestatic`, and the receiver (`this`) is stored
    /// as a field on the continuation class.
    #[serde(default)]
    pub is_instance_method: bool,
}

/// One suspend-call site in a multi-suspension-point state machine.
/// Locations are MIR-level (`block_idx` + `stmt_idx`) so the JVM
/// backend can walk the body linearly, emitting the segment up to
/// each site and then the canonical spill / set-label / invoke /
/// check-SUSPENDED sequence in its place.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuspendCallSite {
    /// Index of the block containing the suspend call.
    pub block_idx: u32,
    /// Index of the `Stmt::Assign` within that block that *is* the
    /// suspend call (i.e. `Rvalue::Call { kind: Static(...), .. }`
    /// whose callee is a suspend fun).
    pub stmt_idx: u32,
    /// JVM internal name of the class that owns the callee suspend
    /// function. For same-file suspends this is the wrapper class.
    pub callee_class: String,
    /// Source-level name of the callee suspend function. The callee
    /// descriptor is built from [`SuspendCallSite::arg_tys`] plus the
    /// trailing `Continuation` and the erased `Object` return.
    pub callee_method: String,
    /// MIR locals holding the user-supplied arguments to this suspend
    /// call, in source order (i.e. excluding the trailing
    /// `$completion`). Empty for no-arg calls like
    /// `yield_()`. The JVM backend loads these onto the
    /// stack before the continuation for the invoke.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<LocalId>,
    /// Per-arg Kotlin/MIR types, paired positionally with
    /// [`SuspendCallSite::args`]. Backends use these to build the
    /// JVM method descriptor (and to decide whether to load a
    /// primitive slot vs a reference slot).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arg_tys: Vec<Ty>,
    /// Declared return type of the callee suspend fun, **before**
    /// the CPS `Object`-rewrite. After the invoke returns the
    /// boxed result, the backend emits `checkcast` against this type
    /// (for `Unit`/`Nothing` / non-reference slots no checkcast is
    /// emitted). Defaults to `Unit` for backwards compat with the
    /// the single-/multi-suspension shapes (their callees all return `Unit`).
    #[serde(default = "default_return_ty_unit")]
    pub return_ty: Ty,
    /// True if the suspend call dispatches through `invokeinterface`
    /// (e.g. `Deferred.await()`) rather than `invokestatic`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_virtual: bool,
    /// MIR local that receives the (post-checkcast) result of this
    /// suspend call. For no-arg Unit-returning calls this local is
    /// dead (we never load from it), matching the single-/multi-suspension shape.
    /// For calls with a user-visible return value the
    /// backend stores the checkcast'd Object into this slot.
    #[serde(default = "default_result_local_zero")]
    pub result_local: LocalId,
    /// Locals that are live across this suspend call — i.e. must be
    /// spilled to continuation fields before the invoke and
    /// restored from them on resume. Each entry pairs a MIR local id
    /// with the index of its spill slot in [`SuspendStateMachine::spill_layout`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub live_spills: Vec<LiveSpill>,
}

fn default_return_ty_unit() -> Ty {
    Ty::Unit
}

fn default_result_local_zero() -> LocalId {
    LocalId(0)
}

/// A (local, spill-slot) pair recorded on a [`SuspendCallSite`].
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct LiveSpill {
    /// MIR local being spilled.
    pub local: LocalId,
    /// Index into [`SuspendStateMachine::spill_layout`].
    pub slot: u32,
}

/// A synthetic continuation-class field reserved for a local that
/// lives across at least one suspend call. The `kind` decides the
/// JVM type descriptor (`I`, `J`, `D`, `F`, or `Ljava/lang/Object;`)
/// and the naming prefix (`I$`, `J$`, etc.).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpillSlot {
    /// Kotlinc-style name, e.g. `"I$0"`, `"L$2"`, `"J$0"`. Backends
    /// use this as-is for the field name.
    pub name: String,
    /// JVM type category of this slot.
    pub kind: SpillKind,
}

/// Type category of a [`SpillSlot`]. Determines both the JVM
/// descriptor and the kotlinc naming prefix.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpillKind {
    /// `I$n` — `int`, `boolean`.
    Int,
    /// `J$n` — `long`.
    Long,
    /// `F$n` — `float`.
    Float,
    /// `D$n` — `double`.
    Double,
    /// `L$n` — any reference (`Object`, `String`, class types, nullables).
    Ref,
}

impl SpillKind {
    /// Field name prefix, e.g. `"I$"`, `"L$"`.
    pub fn prefix(self) -> &'static str {
        match self {
            SpillKind::Int => "I$",
            SpillKind::Long => "J$",
            SpillKind::Float => "F$",
            SpillKind::Double => "D$",
            SpillKind::Ref => "L$",
        }
    }
    /// JVM field descriptor, e.g. `"I"`, `"Ljava/lang/Object;"`.
    pub fn descriptor(self) -> &'static str {
        match self {
            SpillKind::Int => "I",
            SpillKind::Long => "J",
            SpillKind::Float => "F",
            SpillKind::Double => "D",
            SpillKind::Ref => "Ljava/lang/Object;",
        }
    }
    /// Classify a [`Ty`] into the category backends use for spill slots.
    /// Matches kotlinc's behavior: `bool` widens to `int`, function
    /// types and arrays become `Ref`.
    pub fn for_ty(ty: &Ty) -> SpillKind {
        match ty {
            Ty::Bool | Ty::Int => SpillKind::Int,
            Ty::Long => SpillKind::Long,
            Ty::Double => SpillKind::Double,
            _ => SpillKind::Ref,
        }
    }
}

/// An exception handler entry, mapping a range of try-body blocks to a
/// catch handler block. Backends translate block indices to bytecode
/// offsets when emitting exception tables.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExceptionHandler {
    /// First block of the try body (inclusive).
    pub try_start_block: u32,
    /// Block immediately after the last try-body block (exclusive).
    pub try_end_block: u32,
    /// Block index of the catch handler.
    pub handler_block: u32,
    /// JVM internal name of the caught exception class, e.g.
    /// `"java/lang/ArithmeticException"`. `None` means catch-all.
    pub catch_type: Option<String>,
}

impl MirFunction {
    pub fn new_local(&mut self, ty: Ty) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(ty);
        id
    }
}

/// One MIR-level translation unit. Backends consume an entire
/// `MirModule` at once and produce one (or more) class files / DEX
/// files / `.ll` files.
/// A user-defined class in MIR.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirClass {
    pub name: String,
    /// JVM internal name of the superclass (e.g. `"Animal"`), or `None` for `java/lang/Object`.
    pub super_class: Option<String>,
    pub is_open: bool,
    pub is_abstract: bool,
    pub is_interface: bool,
    /// Interfaces this class implements (JVM internal names).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<String>,
    pub fields: Vec<MirField>,
    pub methods: Vec<MirFunction>,
    /// The `<init>` constructor method (primary constructor).
    pub constructor: MirFunction,
    /// Secondary constructors — additional `<init>` methods with different signatures.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondary_constructors: Vec<MirFunction>,
    /// True when the Kotlin source actually declared a primary
    /// constructor (parenthesised parameter list on the class header,
    /// even if empty). When `false`, mir-lower still populates
    /// [`MirClass::constructor`] with a synthesized no-arg shell so
    /// JVM bytecode emission stays uniform — but the metadata writer
    /// must suppress that shell from the `@Metadata` `B`-records when
    /// the class also declares one or more explicit secondaries, to
    /// avoid a duplicate `<init>()V` `Constructor` proto that trips
    /// kotlinc 2.4's "overload resolution ambiguity" diagnostic on
    /// consumers (e.g. parity/101-hash MD5).
    ///
    /// Defaults to `true` so existing fixtures (whose ctor shape never
    /// includes "no-primary + only secondaries") are unchanged.
    #[serde(
        default = "skotch_default_true",
        skip_serializing_if = "skotch_is_true"
    )]
    pub has_explicit_primary_ctor: bool,
    /// True for synthetic lambda classes whose body
    /// contains a suspend call (or are declared `suspend {}`). When
    /// set, the JVM backend emits the class as a subclass of
    /// `kotlin/coroutines/jvm/internal/SuspendLambda` with the
    /// canonical 5-method shape (ctor, invokeSuspend, create, typed
    /// invoke, bridge invoke).
    ///
    /// ## invokeSuspend body
    ///
    /// The first method in [`MirClass::methods`] is the lambda's
    /// `invoke` fn as built by the MIR lowerer. For suspend lambdas,
    /// that fn's `suspend_state_machine` marker tells the JVM backend
    /// how to emit the real CPS state machine into
    /// `invokeSuspend(Object)Object`. Initially the body was an
    /// `IllegalStateException` stub; it is now replaced with a
    /// kotlinc-shaped state machine for 0 or 1 suspension points.
    /// Richer shapes (multi-suspension, capture-crossing locals,
    /// non-literal tails) fall back to the stub and are tracked as
    /// future work.
    ///
    /// Non-suspend lambdas keep the standard `$Lambda$N` shape
    /// (Function1-only, direct invoke) byte-stable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_suspend_lambda: bool,
    /// True when this class was synthesized from a Kotlin lambda
    /// expression by the MIR lambda-lifting pass. Used by the JVM
    /// backend and MIR-lower to recognize lambda classes without
    /// relying on substring matches against the class name. The
    /// class name itself follows kotlinc's `<wrapper>$<fn>$<idx>`
    /// shape so it doesn't carry a structural marker.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_lambda: bool,
    /// True for stub entries from cross-file compilation. The stub provides
    /// field/method metadata for the MIR lowerer but should NOT be emitted
    /// as a class file by backends (the real class comes from another file).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_cross_file_stub: bool,
    /// Annotations on this class (emitted as RuntimeVisibleAnnotations).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<MirAnnotation>,
    /// True when the source class declaration had type parameters
    /// (`class Box<T>(...)`). Methods whose param or return types
    /// reference this class need a JVMS §4.7.9 Signature attribute on
    /// the method, even when the method itself has no type parameters
    /// (fixture 367: `fun printHolder(h: Holder<*>)`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_type_params: bool,
    /// True when this class was synthesized from a Kotlin `object`
    /// declaration. The JVM backend emits a static `INSTANCE` field
    /// (typed as `LClassName;`) plus a `<clinit>` that constructs and
    /// stores the singleton. Call sites in MIR-lower emit
    /// `getstatic INSTANCE; invokevirtual method` instead of routing
    /// through a top-level static helper (fixtures 28/68/325/326/532).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_object_singleton: bool,
    /// When `Some(name)`, this class has a Kotlin `companion object`
    /// lowered to a sibling MirClass named `name` (typically
    /// `<OuterName>$Companion`). The JVM backend emits a static
    /// `Companion` field on this outer class (typed `L<name>;`) plus
    /// initialization in `<clinit>`. MIR-lower routes
    /// `<OuterName>.<companionMethod>(...)` through `getstatic
    /// Companion; invokevirtual <name>.<method>` (fixtures 27/69/512
    /// et al.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion_class_name: Option<std::string::String>,
    /// `static final` fields on this class, initialized in [`MirClass::clinit`]
    /// rather than the instance constructor. Used for Kotlin `enum` entries,
    /// which kotlinc emits as `public static final <Enum> ENTRY;` singletons
    /// constructed once in `<clinit>` so reference equality (`==`) on entries
    /// works (fixtures 65/427/995 et al.). The JVM/DEX backends emit these with
    /// `ACC_STATIC | ACC_FINAL | ACC_ENUM`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub static_fields: Vec<MirField>,
    /// A synthesized `<clinit>()V` static initializer for this class, emitted
    /// when [`MirClass::static_fields`] need runtime construction (enum entries).
    /// Its body is ordinary MIR (`NewInstance` + `Constructor` + `PutStaticField`)
    /// so backends reuse their normal instruction emission. Backends treat the
    /// method as `ACC_STATIC` with no implicit `this`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clinit: Option<MirFunction>,
    /// True when the source class declaration carried both the
    /// `@JvmInline` annotation and the soft `value` modifier (the Kotlin
    /// 1.5+ inline value-class shape — `@JvmInline value class UserId(val
    /// raw: Long)`). Phase H1 only records this bit; subsequent phases
    /// (H2 backend, H3 call-site erasure, H4 metadata) consume it to
    /// emit the kotlinc-shaped erased ABI (private `<init>`, static
    /// `Companion.box-impl`/`unbox-impl`, mangled instance methods that
    /// erase to operations on the underlying value). Until those phases
    /// land, value classes still emit as ordinary wrapper classes — the
    /// flag is informational only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_value_class: bool,
    /// When [`MirClass::is_value_class`] is set, this is the name of
    /// the primary-constructor `val` parameter that holds the
    /// underlying value (e.g. `"raw"` for
    /// `value class UserId(val raw: Long)`). Phase H2+ consumes this to
    /// emit the synthesized `unbox-impl()` accessor + static method
    /// signatures that take the underlying type instead of `this`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_underlying_field: Option<std::string::String>,
    /// When [`MirClass::is_value_class`] is set, this is the [`Ty`] of
    /// the underlying primary-ctor `val` parameter (e.g. [`Ty::Long`]
    /// for `value class UserId(val raw: Long)`). Phase H2+ uses it to
    /// build the erased method descriptors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_underlying_ty: Option<Ty>,
}

/// A field in a MIR class.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirField {
    pub name: String,
    pub ty: Ty,
    /// True when the source declaration carried the `@JvmField`
    /// annotation. Backends use this to emit the field as a direct
    /// `public` member (no synthesized `getX`/`setX` accessor pair),
    /// matching kotlinc's @JvmField interop ABI. Defaults to false
    /// so existing fixtures are unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_jvm_field: bool,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct MirModule {
    /// Wrapper class name for top-level functions in this file
    /// (e.g. `HelloKt` for a file named `Hello.kt`).
    pub wrapper_class: String,
    pub functions: Vec<MirFunction>,
    /// User-defined classes.
    ///
    /// Multiple entries may share a name when a real MirClass from
    /// this file co-exists with a cross-file stub for the same JVM
    /// name (the stub is registered from `package_symbols` before the
    /// owning file is lowered). For lookups, prefer
    /// [`MirModule::find_class`] and friends — they consult the
    /// name→indices index and handle the prefer-non-stub semantics.
    pub classes: Vec<MirClass>,
    /// Name → indices into `classes`. Maintained by
    /// [`MirModule::push_class`]; callers that mutate `classes`
    /// directly (rare) must call [`MirModule::rebuild_class_index`]
    /// afterward. Skipped during serialization — rebuilt on load.
    #[serde(skip)]
    pub classes_by_name: rustc_hash::FxHashMap<String, Vec<u32>>,
    /// Insertion-order stable string pool. Backends iterate this in
    /// order to lay out their constant pool / string id table.
    pub strings: Vec<String>,
    /// Names of enum classes (mapped to String type for parameter resolution).
    #[serde(default, skip_serializing_if = "rustc_hash::FxHashSet::is_empty")]
    pub enum_names: rustc_hash::FxHashSet<String>,
    /// Maps an enum-entry accessor FuncId → (enum_class, entry_name).
    /// kotlinc resolves `Color.RED` directly to `getstatic Color.RED:LColor;`
    /// — it does NOT emit a `RED()LColor;` accessor on the wrapper class. We
    /// still synthesize the accessor MirFunction so generic call-site
    /// resolution (Static dispatch via `name_to_func`) doesn't need a
    /// special case, but every call-site lowering checks this map first and
    /// inlines a `GetStaticField` rvalue instead, and the JVM backend skips
    /// these FuncIds entirely.
    #[serde(default, skip_serializing_if = "rustc_hash::FxHashMap::is_empty")]
    pub enum_entry_funcs: rustc_hash::FxHashMap<u32, (String, String)>,
    /// Maps a cross-file fn stub FuncId →
    /// `(owner_class, method_name, descriptor)`. mir-lower registers a
    /// stub MirFunction for every top-level fn declared in a sibling
    /// file (via the gathered `PackageSymbolTable`) so the body walker
    /// can resolve `someOtherFileFn(args)` as a normal `Static(FuncId)`
    /// call. The JVM backend skips emitting any method body for these
    /// FuncIds and reroutes the `invokestatic` to the recorded owner
    /// class + name + descriptor at call sites. Mirrors the
    /// [`enum_entry_funcs`](Self::enum_entry_funcs) pattern.
    #[serde(default, skip_serializing_if = "rustc_hash::FxHashMap::is_empty")]
    pub cross_file_fn_stubs: rustc_hash::FxHashMap<u32, (String, String, String)>,
    /// Transient: while a Companion class's method bodies are being
    /// lowered, this holds `(companion_class_name, method_names)` so
    /// the bare-call resolver can find sibling overloads even though
    /// the Companion isn't yet in `module.classes`. Set by `lower_class`
    /// at the start of the companion-method loop and cleared after. The
    /// resolver checks `module.classes` first, then falls back to this.
    #[serde(skip)]
    pub pending_companion_methods: Option<(String, Vec<String>)>,
    /// Transient flag: when true, the NEXT lambda lowered should be
    /// a SuspendLambda. Set by coroutine builder handlers (runBlocking,
    /// launch, async) before their trailing lambda arg is lowered.
    /// Cleared by the Expr::Lambda handler after use.
    #[serde(skip)]
    pub force_suspend_lambda: bool,
    /// When set, the next lambda creation should treat its first invoke
    /// param as a `this` receiver of this type. Used for extension
    /// function types like `StringBuilder.() -> Unit`. Set by call-site
    /// handlers before the lambda arg is lowered; cleared after use.
    #[serde(skip)]
    pub lambda_receiver_type: Option<String>,
    /// When set, the next lambda's implicit `it` parameter (and any
    /// explicit single-name param) is typed to this `Ty` instead of
    /// the default `Ty::Any`. Used for collection methods that know
    /// their element type — `list.filter { it.foo }` propagates the
    /// list's element type into the lambda so `it.foo` resolves
    /// against the right class. Set by call-site handlers before the
    /// lambda arg is lowered; cleared after use.
    #[serde(skip)]
    pub lambda_param_type: Option<Ty>,
    /// When set, the next lambda's explicit user parameters are typed
    /// from this slice (indexed by user-param position, capture params
    /// not counted). Used for multi-arg HOFs whose lambda shape can't
    /// be expressed by `lambda_param_type` alone — e.g.
    /// `Iterable<T>.fold(initial: R, op: (R, T) -> R)` needs
    /// `[R, T]` so the lambda body sees `acc` and `s` with concrete
    /// types and `s.area()` resolves against the element class.
    /// Entries with `Ty::Any` fall back to the source annotation;
    /// non-Any entries override it. Cleared after use.
    #[serde(skip)]
    pub lambda_param_types: Option<Vec<Ty>>,
    /// Type alias mappings: alias name → target type name.
    #[serde(default, skip_serializing_if = "rustc_hash::FxHashMap::is_empty")]
    pub type_aliases: rustc_hash::FxHashMap<String, String>,
    /// Function-typed typealiases — alias name → fully resolved
    /// `Ty::Function`. Required because `type_aliases` above is just
    /// a name→name map (the target's simple name), which for a
    /// function-type target is only its return type's name — losing
    /// the function-type structure. `resolve_type_ref` consults this
    /// before falling back to the name-only resolution. Skipped
    /// during serialization (rebuilt each compile).
    #[serde(skip)]
    pub function_aliases: rustc_hash::FxHashMap<String, Ty>,
    /// Import map: simple class name → JVM internal path.
    /// Built from `import` declarations and default java.lang.* imports.
    /// Used to resolve bare class names like `Random` to `java/util/Random`.
    /// Skipped during serialization since it's only needed at lowering time.
    #[serde(skip)]
    pub import_map: rustc_hash::FxHashMap<String, String>,
    /// Static method imports: method name → (class_jvm_path, method_name).
    /// E.g. `import org.junit.jupiter.api.Assertions.assertTrue` maps
    /// `"assertTrue" → ("org/junit/jupiter/api/Assertions", "assertTrue")`.
    #[serde(skip)]
    pub static_method_imports: rustc_hash::FxHashMap<String, (String, String)>,
    /// Cross-file function declarations from the same compilation unit.
    /// Populated during multi-file lowering. Maps function name →
    /// (owner_class, descriptor, return_ty). Skipped during serialization.
    #[serde(skip)]
    pub cross_file_fns: rustc_hash::FxHashMap<String, (String, String, skotch_types::Ty)>,
    /// Per-cross-file-fn additional metadata. Keyed by the same name
    /// the caller uses for `cross_file_fns`. Callers that need to know
    /// whether the function was declared `inline`, has an extension
    /// receiver, or has per-parameter defaults consult this side
    /// table — keeping `cross_file_fns`'s tuple shape stable for
    /// existing consumers. Skipped during serialization.
    #[serde(skip)]
    pub cross_file_fn_extras: rustc_hash::FxHashMap<String, CrossFileFnExtras>,
    /// All cross-file function overloads keyed by name. The simpler
    /// `cross_file_fns` map collapses overloads to a single entry —
    /// fine for unique-name top-level fns, but it loses information
    /// for overloaded names like `internal operator fun X.get(...)`
    /// vs `internal operator fun Y.get(...)`. Lookup sites that
    /// distinguish between overloads (such as the index-operator
    /// dispatcher) iterate this list and pick the entry whose
    /// receiver type matches.
    #[serde(skip)]
    pub cross_file_fn_overloads:
        rustc_hash::FxHashMap<String, Vec<(String, String, skotch_types::Ty, CrossFileFnExtras)>>,
    /// Cross-file class declarations. Maps simple class name →
    /// (jvm_name, kind_str, is_data_class). Used for constructor calls.
    #[serde(skip)]
    pub cross_file_classes: rustc_hash::FxHashMap<String, (String, String, bool)>,
    /// Cross-file top-level val declarations. Maps val name →
    /// (owner_class, ty). Top-level vals in Kotlin compile to a static
    /// field plus a static `get<Name>()` accessor on the file's wrapper
    /// class. Call sites that reference these (e.g. `unreadMessages.toList()`)
    /// need to invoke the accessor before chaining the method call.
    #[serde(skip)]
    pub cross_file_vals: rustc_hash::FxHashMap<String, (String, skotch_types::Ty)>,
    /// Top-level `const val` declarations to be emitted as static final
    /// fields in the wrapper class. Each tuple is `(name, ty, value)`.
    /// kotlinc emits these as `public static final` fields with a
    /// ConstantValue attribute.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_level_consts: Vec<(String, Ty, MirConst)>,
    /// Non-`const val` top-level properties (e.g. `val GREETING = "hi"`).
    /// kotlinc emits these as `private static final` fields initialized
    /// in `<clinit>`, with public `get<Name>()` accessor methods. Use
    /// sites read them via `getstatic` (no inlining).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_level_props: Vec<(String, Ty, MirConst)>,
    /// Names (as strings — same as kotlinc's `field.name`) of top-level
    /// non-`const val` properties. Used by the lowerer's Ident lookup
    /// to emit `Rvalue::GetStaticField` instead of inlining the literal.
    #[serde(default, skip_serializing_if = "rustc_hash::FxHashSet::is_empty")]
    pub top_level_prop_names: rustc_hash::FxHashSet<String>,
}

impl MirModule {
    pub fn intern_string(&mut self, s: &str) -> StringId {
        if let Some(idx) = self.strings.iter().position(|t| t == s) {
            return StringId(idx as u32);
        }
        let id = StringId(self.strings.len() as u32);
        self.strings.push(s.to_string());
        id
    }

    pub fn add_function(&mut self, mut f: MirFunction) -> FuncId {
        let id = FuncId(self.functions.len() as u32);
        f.id = id;
        self.functions.push(f);
        id
    }

    pub fn lookup_string(&self, id: StringId) -> &str {
        &self.strings[id.0 as usize]
    }

    /// Whether the named class is a synthetic lambda (lifted from a
    /// Kotlin lambda expression). Replaces the legacy
    /// `name.contains("$Lambda$")` substring check now that lambda
    /// classes follow kotlinc's `<wrapper>$<fn>$<idx>` naming.
    pub fn is_lambda_class(&self, name: &str) -> bool {
        self.classes_with_name(name)
            .any(|c| c.is_lambda || c.is_suspend_lambda)
    }

    /// Push a class and keep `classes_by_name` in sync. Prefer this
    /// over `module.classes.push(c)` so the name index stays valid.
    pub fn push_class(&mut self, class: MirClass) {
        let idx = self.classes.len() as u32;
        let name = class.name.clone();
        self.classes.push(class);
        self.classes_by_name.entry(name).or_default().push(idx);
    }

    /// Rebuild `classes_by_name` from scratch. Used after batch
    /// mutations to `classes` that bypassed [`push_class`], and after
    /// deserialization (where the index isn't persisted).
    pub fn rebuild_class_index(&mut self) {
        self.classes_by_name.clear();
        for (i, c) in self.classes.iter().enumerate() {
            self.classes_by_name
                .entry(c.name.clone())
                .or_default()
                .push(i as u32);
        }
    }

    /// Return all `MirClass` entries matching `name`. Multiple matches
    /// happen when a cross-file stub coexists with the real class.
    pub fn classes_with_name<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Iterator<Item = &'a MirClass> + 'a {
        let indices = self.classes_by_name.get(name);
        indices
            .into_iter()
            .flat_map(|v| v.iter())
            .map(move |&i| &self.classes[i as usize])
    }

    /// First-class match for `name`, or `None`. Prefers a non-stub
    /// entry when both a real class and a cross-file stub exist for
    /// the same JVM name.
    pub fn find_class(&self, name: &str) -> Option<&MirClass> {
        let indices = self.classes_by_name.get(name)?;
        // Two-pass: non-stub first, else first stub. Cheap because
        // `indices.len() <= 2` in practice (real + stub).
        indices
            .iter()
            .map(|&i| &self.classes[i as usize])
            .find(|c| !c.is_cross_file_stub)
            .or_else(|| indices.first().map(|&i| &self.classes[i as usize]))
    }

    /// Mutable counterpart to `find_class`. Same prefer-non-stub
    /// semantics.
    pub fn find_class_mut(&mut self, name: &str) -> Option<&mut MirClass> {
        let indices = self.classes_by_name.get(name)?.clone();
        let preferred = indices
            .iter()
            .copied()
            .find(|&i| !self.classes[i as usize].is_cross_file_stub);
        let idx = preferred.or_else(|| indices.first().copied())?;
        Some(&mut self.classes[idx as usize])
    }

    /// Does ANY class entry match this name? O(1) common-case check
    /// when the caller only needs existence (very common in dispatch
    /// path predicates).
    pub fn has_class(&self, name: &str) -> bool {
        self.classes_by_name.contains_key(name)
    }

    /// Resolve a simple class name to its FQ JVM-internal name by
    /// consulting (in priority order): the name itself if it already
    /// contains `/` (assumed FQ), the file's import map, the
    /// cross-file class registry, and finally the simple name as a
    /// fallback. Centralizes the resolution chain that lower_class /
    /// lower_object / lower_enum / lower_interface and the cross-file
    /// stub builder used to inline at 6+ sites.
    pub fn fq_resolve(&self, simple: &str) -> String {
        if simple.contains('/') {
            return simple.to_string();
        }
        if let Some(fq) = self.import_map.get(simple) {
            return fq.clone();
        }
        if let Some((fq, _, _)) = self.cross_file_classes.get(simple) {
            return fq.clone();
        }
        simple.to_string()
    }

    /// Same as [`fq_resolve`] but takes an `Option<&str>` and returns
    /// `Option<String>`. Convenient for resolving optional super_class
    /// slots without wrapping in `map`.
    pub fn fq_resolve_opt<S: AsRef<str>>(&self, simple: Option<S>) -> Option<String> {
        simple.map(|s| self.fq_resolve(s.as_ref()))
    }
}

/// Module-free heuristic for "does this class name LOOK like a
/// kotlinc-emitted lambda" — used by passes that don't have a
/// `MirModule` reference. The shape is `<wrapper>$<fn>$<digits>` (or
/// nested `$<digits>$<digits>`). Matches both the new naming and any
/// historical `$Lambda$N` form.
pub fn looks_like_lambda_class_name(name: &str) -> bool {
    if let Some(idx) = name.rfind('$') {
        let suffix = &name[idx + 1..];
        if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
            // Need at least one more `$` before the digit suffix so we
            // don't accidentally match `Foo$1` (legacy local class).
            // The pattern requires `<wrapper>$<fn>$<idx>` — two dollar
            // signs minimum.
            let prefix = &name[..idx];
            return prefix.contains('$');
        }
    }
    false
}
