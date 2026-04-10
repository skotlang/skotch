//! Textual LLVM IR emitter for skot.
//!
//! Like the rest of skot, this backend takes a [`MirModule`] and emits
//! a target-format text blob — in this case `.ll`-formatted LLVM IR.
//! It deliberately does **not** depend on `inkwell`/`llvm-sys`/
//! `libLLVM`. The whole pipeline is plain string formatting:
//!
//! ```text
//!     MirModule  ──►  LLVM IR text  ──►  clang  ──►  native binary
//! ```
//!
//! ## Two entry points
//!
//! - [`compile_module`] takes a [`MirModule`] directly. Used by
//!   in-process callers and tests.
//! - [`compile_klib`] takes a `.klib`'s bytes (as produced by
//!   `skot-backend-klib::write_klib`), reads the embedded MIR, and
//!   then runs the same compilation. This is what `skot emit
//!   --target llvm` exercises so the multi-stage pipeline is the same
//!   one a user-driven build would take.
//!
//! ## Runtime
//!
//! For PR #4 the runtime is **libc**. We don't model the Kotlin
//! standard library — `println(string)` lowers to `puts(str)` and
//! `println(int)` lowers to `printf("%d\n", val)`. Each call site
//! interns the format string into the module's global constants.
//!
//! ## What this currently emits
//!
//! - String constants as `private unnamed_addr constant [N x i8]`
//! - The `puts` / `printf` declarations on first use
//! - One LLVM `define` per top-level Kotlin function
//! - Integer constants via `add i32 0, <value>`
//! - Integer arithmetic via `add`/`sub`/`mul`/`sdiv`/`srem`
//! - `call void @<func>(...)` for inter-function calls
//! - `ret void` (Unit) and `ret i32 0` (main)
//!
//! ## What we punt to later PRs
//!
//! - Optimization passes (we emit naive `add 0, x` for constants)
//! - Branches and `phi` nodes (no `if`-as-expression yet)
//! - Generics, lambdas, classes
//! - Long/Float/Double types

use anyhow::Result;
use skot_mir::{
    BasicBlock, BinOp as MBinOp, CallKind, FuncId, LocalId, MirConst, MirFunction, MirModule,
    Rvalue, Stmt, Terminator,
};
use skot_types::Ty;
use std::fmt::Write as _;

/// Compile a [`MirModule`] to LLVM textual IR.
pub fn compile_module(module: &MirModule) -> String {
    let mut emitter = Emitter::new(module);
    emitter.emit()
}

/// Compile a skot `.klib` (as produced by `skot-backend-klib::write_klib`)
/// to LLVM textual IR. This is the multi-stage entry point — it
/// exercises the same kt → MIR → klib → LLVM path that the kotlinc-native
/// pipeline takes.
pub fn compile_klib(klib_bytes: &[u8]) -> Result<String> {
    let (module, _manifest) = skot_backend_klib::read_klib(klib_bytes)?;
    Ok(compile_module(&module))
}

/// Internal codegen state. Walks the MIR once and emits text into a
/// growing `String`.
struct Emitter<'a> {
    module: &'a MirModule,
    /// String pool entry → global symbol name (e.g. `@.str.0`).
    string_globals: Vec<String>,
    /// Format strings interned by `println(<int>)` etc.
    format_strings: Vec<(String, String)>, // (name, contents)
    out: String,
    needs_puts: bool,
    needs_printf: bool,
}

impl<'a> Emitter<'a> {
    fn new(module: &'a MirModule) -> Self {
        let string_globals = (0..module.strings.len())
            .map(|i| format!("@.str.{i}"))
            .collect();
        Emitter {
            module,
            string_globals,
            format_strings: Vec::new(),
            out: String::new(),
            needs_puts: false,
            needs_printf: false,
        }
    }

    fn emit(&mut self) -> String {
        // First pass: discover what runtime declarations we need by
        // scanning the function bodies. This lets us emit the
        // declarations once at the top of the module.
        for func in &self.module.functions {
            self.scan_runtime_needs(func);
        }

        // Header.
        writeln!(self.out, "; ModuleID = '{}'", self.module.wrapper_class).unwrap();
        writeln!(
            self.out,
            "source_filename = \"{}.kt\"",
            self.module.wrapper_class
        )
        .unwrap();
        writeln!(self.out).unwrap();

        // String constants.
        for (i, s) in self.module.strings.iter().enumerate() {
            self.emit_c_string(&format!("@.str.{i}"), s);
        }

        // Pre-intern format strings used by println(int).
        let int_fmt = self.intern_format_string("int_println", "%d\n");
        let _ = int_fmt; // emitted lazily as needed

        // Format string globals.
        for (name, contents) in &self.format_strings.clone() {
            self.emit_c_string(name, contents);
        }
        if !self.format_strings.is_empty() || !self.module.strings.is_empty() {
            writeln!(self.out).unwrap();
        }

        // Runtime declarations.
        if self.needs_puts {
            writeln!(self.out, "declare i32 @puts(ptr)").unwrap();
        }
        if self.needs_printf {
            writeln!(self.out, "declare i32 @printf(ptr, ...)").unwrap();
        }
        if self.needs_puts || self.needs_printf {
            writeln!(self.out).unwrap();
        }

        // Functions in source order.
        for func in &self.module.functions {
            self.emit_function(func);
            writeln!(self.out).unwrap();
        }
        std::mem::take(&mut self.out)
    }

    fn scan_runtime_needs(&mut self, func: &MirFunction) {
        for block in &func.blocks {
            for stmt in &block.stmts {
                let Stmt::Assign { value, .. } = stmt;
                if let Rvalue::Call {
                    kind: CallKind::Println,
                    args,
                } = value
                {
                    if let Some(&arg) = args.first() {
                        let ty = &func.locals[arg.0 as usize];
                        match ty {
                            Ty::String => self.needs_puts = true,
                            Ty::Int | Ty::Bool => self.needs_printf = true,
                            _ => self.needs_puts = true,
                        }
                    } else {
                        self.needs_puts = true;
                    }
                }
            }
        }
    }

    fn intern_format_string(&mut self, label: &str, contents: &str) -> String {
        let name = format!("@.fmt.{label}");
        if !self.format_strings.iter().any(|(n, _)| n == &name) {
            self.format_strings
                .push((name.clone(), contents.to_string()));
        }
        name
    }

    /// Emit a C-style global string constant. Length is byte-count
    /// including the trailing NUL.
    fn emit_c_string(&mut self, name: &str, value: &str) {
        let bytes = value.as_bytes();
        let len = bytes.len() + 1;
        let escaped = escape_c_string(value);
        writeln!(
            self.out,
            "{name} = private unnamed_addr constant [{len} x i8] c\"{escaped}\\00\", align 1"
        )
        .unwrap();
    }

    fn emit_function(&mut self, func: &MirFunction) {
        let llvm_name = llvm_name_for(&self.module.wrapper_class, &func.name);
        let is_main = func.name == "main";
        let return_type = if is_main {
            "i32"
        } else {
            llvm_type(&func.return_ty)
        };

        // Parameter list. Locals 0..num_params are parameters; the
        // rest get allocated as %tmp_<id> SSA registers inside the body.
        let mut params_text = String::new();
        for (i, &p) in func.params.iter().enumerate() {
            if i > 0 {
                params_text.push_str(", ");
            }
            let ty = &func.locals[p.0 as usize];
            params_text.push_str(llvm_type(ty));
            params_text.push_str(&format!(" %arg{}", p.0));
        }

        writeln!(
            self.out,
            "define {return_type} @{llvm_name}({params_text}) {{"
        )
        .unwrap();
        writeln!(self.out, "entry:").unwrap();

        let block: &BasicBlock = &func.blocks[0];
        let mut walker = BlockWalker {
            module: self.module,
            func,
            string_globals: &self.string_globals,
            out: &mut self.out,
            ssa_for_local: vec![None; func.locals.len()],
            next_tmp: 0,
        };
        // Bind parameters: each parameter local needs an SSA name.
        // We pre-populate ssa_for_local for parameters with the
        // arg-named SSA value.
        for &p in &func.params {
            walker.ssa_for_local[p.0 as usize] = Some(format!("%arg{}", p.0));
        }
        walker.walk_block(block);

        // Terminator. Main always returns 0.
        match &block.terminator {
            Terminator::Return | Terminator::ReturnValue(_) => {
                if is_main {
                    writeln!(self.out, "  ret i32 0").unwrap();
                } else {
                    writeln!(self.out, "  ret void").unwrap();
                }
            }
        }
        writeln!(self.out, "}}").unwrap();
    }
}

/// Mid-walk codegen state — kept separate from `Emitter` so the
/// borrow checker is happy about borrowing `out` mutably while still
/// reading from `module`.
struct BlockWalker<'a> {
    module: &'a MirModule,
    func: &'a MirFunction,
    string_globals: &'a [String],
    out: &'a mut String,
    /// For each MIR LocalId, the LLVM SSA value name that holds its
    /// current value (e.g. `%tmp3`, `%arg1`, or a literal). `None`
    /// means the local hasn't been materialized yet.
    ssa_for_local: Vec<Option<String>>,
    next_tmp: u32,
}

impl<'a> BlockWalker<'a> {
    fn fresh(&mut self) -> String {
        let s = format!("%t{}", self.next_tmp);
        self.next_tmp += 1;
        s
    }

    fn ssa_of(&self, local: LocalId) -> String {
        self.ssa_for_local[local.0 as usize]
            .clone()
            .unwrap_or_else(|| panic!("local {:?} used before assignment", local))
    }

    fn walk_block(&mut self, block: &BasicBlock) {
        for stmt in &block.stmts {
            let Stmt::Assign { dest, value } = stmt;
            self.lower_assign(*dest, value);
        }
    }

    fn lower_assign(&mut self, dest: LocalId, rvalue: &Rvalue) {
        let dest_ty = &self.func.locals[dest.0 as usize];
        if matches!(dest_ty, Ty::Unit) {
            // Unit-typed assignments still need to issue the call,
            // because the call has side effects. We just don't bind
            // an SSA name.
            if let Rvalue::Call { .. } = rvalue {
                self.lower_call_void(rvalue);
            }
            return;
        }
        match rvalue {
            Rvalue::Const(c) => self.lower_const(dest, c),
            Rvalue::Local(src) => {
                // SSA copy: bind dest to the same SSA name as src.
                let s = self.ssa_of(*src);
                self.ssa_for_local[dest.0 as usize] = Some(s);
            }
            Rvalue::BinOp { op, lhs, rhs } => {
                let l = self.ssa_of(*lhs);
                let r = self.ssa_of(*rhs);
                let opcode = match op {
                    MBinOp::AddI => "add",
                    MBinOp::SubI => "sub",
                    MBinOp::MulI => "mul",
                    MBinOp::DivI => "sdiv",
                    MBinOp::ModI => "srem",
                };
                let dst = self.fresh();
                writeln!(self.out, "  {dst} = {opcode} i32 {l}, {r}").unwrap();
                self.ssa_for_local[dest.0 as usize] = Some(dst);
            }
            Rvalue::Call { .. } => {
                // Non-Unit calls aren't produced by PR #4 lowering.
                self.lower_call_void(rvalue);
            }
        }
    }

    fn lower_const(&mut self, dest: LocalId, c: &MirConst) {
        match c {
            MirConst::Unit => {}
            MirConst::Bool(b) => {
                // Materialize as `add i32 0, X` so we have an SSA
                // value to bind. LLVM's optimizer collapses this.
                let dst = self.fresh();
                writeln!(self.out, "  {dst} = add i32 0, {}", if *b { 1 } else { 0 }).unwrap();
                self.ssa_for_local[dest.0 as usize] = Some(dst);
            }
            MirConst::Int(v) => {
                let dst = self.fresh();
                writeln!(self.out, "  {dst} = add i32 0, {v}").unwrap();
                self.ssa_for_local[dest.0 as usize] = Some(dst);
            }
            MirConst::String(sid) => {
                // String constants are referenced as @.str.<n> globals.
                // We bind the local directly to the global pointer.
                let global = &self.string_globals[sid.0 as usize];
                self.ssa_for_local[dest.0 as usize] = Some(global.clone());
            }
        }
    }

    fn lower_call_void(&mut self, rvalue: &Rvalue) {
        let Rvalue::Call { kind, args } = rvalue else {
            return;
        };
        match kind {
            CallKind::Println => self.lower_println(args),
            CallKind::Static(target_id) => self.lower_static_call(*target_id, args),
        }
    }

    fn lower_println(&mut self, args: &[LocalId]) {
        let Some(&arg) = args.first() else {
            // println() with no args — emit puts of empty string.
            let _ = self.fresh();
            writeln!(self.out, "  call i32 @puts(ptr @.empty)").unwrap();
            return;
        };
        let arg_ty = &self.func.locals[arg.0 as usize];
        let arg_ssa = self.ssa_of(arg);
        let _ = self.fresh();
        match arg_ty {
            Ty::String => {
                writeln!(self.out, "  call i32 @puts(ptr {arg_ssa})").unwrap();
            }
            Ty::Int | Ty::Bool => {
                writeln!(
                    self.out,
                    "  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 {arg_ssa})"
                )
                .unwrap();
            }
            _ => {
                writeln!(self.out, "  call i32 @puts(ptr {arg_ssa})").unwrap();
            }
        }
    }

    fn lower_static_call(&mut self, target_id: FuncId, args: &[LocalId]) {
        let target = &self.module.functions[target_id.0 as usize];
        let llvm_name = llvm_name_for(&self.module.wrapper_class, &target.name);
        let mut arg_text = String::new();
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                arg_text.push_str(", ");
            }
            let ty = &self.func.locals[a.0 as usize];
            let ssa = self.ssa_of(*a);
            arg_text.push_str(llvm_type(ty));
            arg_text.push(' ');
            arg_text.push_str(&ssa);
        }
        writeln!(self.out, "  call void @{llvm_name}({arg_text})").unwrap();
    }
}

fn llvm_type(ty: &Ty) -> &'static str {
    match ty {
        Ty::Unit => "void",
        Ty::Bool => "i1",
        Ty::Int => "i32",
        Ty::Long => "i64",
        Ty::Double => "double",
        Ty::String => "ptr",
        Ty::Any | Ty::Nullable(_) => "ptr",
        Ty::Error => "void",
    }
}

fn llvm_name_for(wrapper_class: &str, name: &str) -> String {
    if name == "main" {
        // The C entry point is `main`; the wrapper class is implicit.
        "main".to_string()
    } else {
        format!("{wrapper_class}_{name}")
    }
}

/// Escape a string for inclusion as a C-style LLVM constant. Only the
/// characters that LLVM IR text actually demands escaping (`"`, `\`,
/// non-printable bytes) are escaped — everything else passes through
/// verbatim.
fn escape_c_string(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        match b {
            b'"' => out.push_str("\\22"),
            b'\\' => out.push_str("\\5C"),
            b'\n' => out.push_str("\\0A"),
            b'\r' => out.push_str("\\0D"),
            b'\t' => out.push_str("\\09"),
            b if (0x20..=0x7e).contains(&b) => out.push(b as char),
            b => {
                let _ = write!(out, "\\{:02X}", b);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use skot_intern::Interner;
    use skot_lexer::lex;
    use skot_mir_lower::lower_file;
    use skot_parser::parse_file;
    use skot_resolve::resolve_file;
    use skot_span::FileId;
    use skot_typeck::type_check;

    fn build(src: &str) -> String {
        let mut interner = Interner::new();
        let mut diags = skot_diagnostics::Diagnostics::new();
        let lf = lex(FileId(0), src, &mut diags);
        let ast = parse_file(&lf, &mut interner, &mut diags);
        let r = resolve_file(&ast, &mut interner, &mut diags);
        let t = type_check(&ast, &r, &mut interner, &mut diags);
        let m = lower_file(&ast, &r, &t, &mut interner, &mut diags, "InputKt");
        assert!(!diags.has_errors(), "{:?}", diags);
        compile_module(&m)
    }

    #[test]
    fn emit_hello_ll_text() {
        let ll = build(r#"fun main() { println("Hello, world!") }"#);
        assert!(ll.contains("@.str.0 = private unnamed_addr constant"));
        assert!(ll.contains("Hello, world!"));
        assert!(ll.contains("declare i32 @puts(ptr)"));
        assert!(ll.contains("define i32 @main()"));
        assert!(ll.contains("call i32 @puts(ptr @.str.0)"));
        assert!(ll.contains("ret i32 0"));
    }

    #[test]
    fn emit_int_println_uses_printf() {
        let ll = build("fun main() { println(42) }");
        assert!(ll.contains("declare i32 @printf(ptr, ...)"));
        assert!(ll.contains("@.fmt.int_println"));
        assert!(ll.contains("call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32"));
    }

    #[test]
    fn emit_arithmetic_uses_add_mul() {
        let ll = build("fun main() { println(1 + 2 * 3) }");
        assert!(ll.contains(" mul i32 "));
        assert!(ll.contains(" add i32 "));
    }

    #[test]
    fn emit_function_call_uses_static_call() {
        let src = r#"
            fun greet(n: String) { println(n) }
            fun main() { greet("Kotlin") }
        "#;
        let ll = build(src);
        assert!(ll.contains("define void @InputKt_greet(ptr"));
        // The main function calls @InputKt_greet.
        assert!(ll.contains("call void @InputKt_greet(ptr @.str.0)"));
    }

    #[test]
    fn klib_round_trip_via_compile_klib() {
        let mut interner = Interner::new();
        let mut diags = skot_diagnostics::Diagnostics::new();
        let lf = lex(
            FileId(0),
            r#"fun main() { println("Hello, world!") }"#,
            &mut diags,
        );
        let ast = parse_file(&lf, &mut interner, &mut diags);
        let r = resolve_file(&ast, &mut interner, &mut diags);
        let t = type_check(&ast, &r, &mut interner, &mut diags);
        let m = lower_file(&ast, &r, &t, &mut interner, &mut diags, "InputKt");
        let klib = skot_backend_klib::write_klib(&m, skot_backend_klib::DEFAULT_TARGET).unwrap();
        let ll = compile_klib(&klib).unwrap();
        assert!(ll.contains("Hello, world!"));
        assert!(ll.contains("call i32 @puts"));
    }

    #[test]
    fn escape_c_string_handles_quotes_and_newlines() {
        assert_eq!(escape_c_string("hi\nthere"), "hi\\0Athere");
        assert_eq!(escape_c_string("\"q\""), "\\22q\\22");
    }
}
