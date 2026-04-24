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

pub mod validate;

fn is_zero_usize(v: &usize) -> bool {
    *v == 0
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
    /// Annotations on this function (emitted as RuntimeVisibleAnnotations).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<MirAnnotation>,
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
    /// True for stub entries from cross-file compilation. The stub provides
    /// field/method metadata for the MIR lowerer but should NOT be emitted
    /// as a class file by backends (the real class comes from another file).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_cross_file_stub: bool,
    /// Annotations on this class (emitted as RuntimeVisibleAnnotations).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<MirAnnotation>,
}

/// A field in a MIR class.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirField {
    pub name: String,
    pub ty: Ty,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct MirModule {
    /// Wrapper class name for top-level functions in this file
    /// (e.g. `HelloKt` for a file named `Hello.kt`).
    pub wrapper_class: String,
    pub functions: Vec<MirFunction>,
    /// User-defined classes.
    pub classes: Vec<MirClass>,
    /// Insertion-order stable string pool. Backends iterate this in
    /// order to lay out their constant pool / string id table.
    pub strings: Vec<String>,
    /// Names of enum classes (mapped to String type for parameter resolution).
    #[serde(default, skip_serializing_if = "rustc_hash::FxHashSet::is_empty")]
    pub enum_names: rustc_hash::FxHashSet<String>,
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
    /// Type alias mappings: alias name → target type name.
    #[serde(default, skip_serializing_if = "rustc_hash::FxHashMap::is_empty")]
    pub type_aliases: rustc_hash::FxHashMap<String, String>,
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
    /// Cross-file class declarations. Maps simple class name →
    /// (jvm_name, kind_str, is_data_class). Used for constructor calls.
    #[serde(skip)]
    pub cross_file_classes: rustc_hash::FxHashMap<String, (String, String, bool)>,
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
}
