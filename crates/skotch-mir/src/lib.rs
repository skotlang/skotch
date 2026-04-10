//! Backend-neutral mid-level IR.
//!
//! MIR is the **narrow waist** between the front-end (lex/parse/resolve/
//! typeck) and the backends (`skotch-backend-jvm`, `-dex`, `-llvm`,
//! `-wasm`). The shape is deliberately small for PR #1: a flat list of
//! basic blocks per function, three-address-code-style assignments
//! into virtual locals, a tiny `Rvalue` enum, and a `Terminator` per
//! block.
//!
//! ## What we model in PR #1
//!
//! - Constant loads (string, int, bool, unit)
//! - Local reads
//! - Calls — either to other top-level functions (`Static`) or to
//!   the hard-coded `Println` intrinsic
//! - Integer arithmetic (`Add`/`Sub`/`Mul`/`Div`/`Mod`)
//! - `Return` and `ReturnValue` terminators
//!
//! ## What we deliberately punt to PR #1.5+
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
    Call {
        kind: CallKind,
        args: Vec<LocalId>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinOp {
    AddI,
    SubI,
    MulI,
    DivI,
    ModI,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CallKind {
    /// Static call to another top-level function in the same module.
    Static(FuncId),
    /// Hard-coded `println` intrinsic. The backend dispatches by the
    /// type of the (single) argument.
    Println,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Stmt {
    Assign { dest: LocalId, value: Rvalue },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Terminator {
    Return,
    ReturnValue(LocalId),
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
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct MirModule {
    /// Wrapper class name for top-level functions in this file
    /// (e.g. `HelloKt` for a file named `Hello.kt`).
    pub wrapper_class: String,
    pub functions: Vec<MirFunction>,
    /// Insertion-order stable string pool. Backends iterate this in
    /// order to lay out their constant pool / string id table.
    pub strings: Vec<String>,
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
