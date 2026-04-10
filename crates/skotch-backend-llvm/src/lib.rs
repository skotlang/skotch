//! Textual LLVM IR emitter for skotch.
//!
//! Like the rest of skotch, this backend takes a [`MirModule`] and emits
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
//!   `skotch-backend-klib::write_klib`), reads the embedded MIR, and
//!   then runs the same compilation. This is what `skotch emit
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
use skotch_mir::{
    BasicBlock, BinOp as MBinOp, CallKind, FuncId, LocalId, MirConst, MirFunction, MirModule,
    Rvalue, Stmt, Terminator,
};
use skotch_types::Ty;
use std::collections::HashMap;
use std::fmt::Write as _;

/// Compile a [`MirModule`] to LLVM textual IR.
pub fn compile_module(module: &MirModule) -> String {
    let mut emitter = Emitter::new(module);
    emitter.emit()
}

/// Compile a skotch `.klib` (as produced by `skotch-backend-klib::write_klib`)
/// to LLVM textual IR. This is the multi-stage entry point — it
/// exercises the same kt → MIR → klib → LLVM path that the kotlinc-native
/// pipeline takes.
pub fn compile_klib(klib_bytes: &[u8]) -> Result<String> {
    let (module, _manifest) = skotch_backend_klib::read_klib(klib_bytes)?;
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
    /// Format strings interned by `PrintlnConcat` template lowering.
    /// Keyed by format-string content so that two call sites with the
    /// same template share one global. Populated during the pre-scan
    /// in [`Emitter::emit`] and looked up at codegen time using the
    /// same format-string-building algorithm.
    concat_format_lookup: HashMap<String, String>, // text → name
    /// Insertion-order list of `(name, format_text)` for the
    /// PrintlnConcat globals, so the emitted IR is deterministic
    /// across runs.
    concat_format_globals: Vec<(String, String)>,
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
            concat_format_lookup: HashMap::new(),
            concat_format_globals: Vec::new(),
            out: String::new(),
            needs_puts: false,
            needs_printf: false,
        }
    }

    fn emit(&mut self) -> String {
        // First pass: discover what runtime declarations we need AND
        // intern every PrintlnConcat format string. Both must happen
        // before we start emitting output, because runtime
        // declarations and global format strings appear at the top
        // of the module.
        for func in &self.module.functions {
            self.scan_runtime_needs(func);
            self.scan_concat_formats(func);
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
        // PrintlnConcat format string globals (deterministic order).
        for (name, contents) in &self.concat_format_globals.clone() {
            self.emit_c_string(name, contents);
        }
        if !self.format_strings.is_empty()
            || !self.concat_format_globals.is_empty()
            || !self.module.strings.is_empty()
        {
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
                match value {
                    Rvalue::Call {
                        kind: CallKind::Println,
                        args,
                    } => {
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
                    Rvalue::Call {
                        kind: CallKind::PrintlnConcat,
                        ..
                    } => {
                        // PrintlnConcat always lowers to a single
                        // printf call.
                        self.needs_printf = true;
                    }
                    _ => {}
                }
            }
        }
    }

    /// Walk one function looking for `CallKind::PrintlnConcat` calls,
    /// and intern a deduped format-string global for each. This is the
    /// pre-scan step that lets the function-body emitter look up
    /// (rather than allocate) globals while it walks.
    fn scan_concat_formats(&mut self, func: &MirFunction) {
        // Track which locals hold a constant string literal, so we
        // know whether each PrintlnConcat arg should be inlined into
        // the format string or referenced via `%s`/`%d`.
        let mut constant_text: Vec<Option<String>> = vec![None; func.locals.len()];
        for block in &func.blocks {
            for stmt in &block.stmts {
                let Stmt::Assign { dest, value } = stmt;
                match value {
                    Rvalue::Const(MirConst::String(sid)) => {
                        constant_text[dest.0 as usize] =
                            Some(self.module.lookup_string(*sid).to_string());
                    }
                    Rvalue::Local(src) => {
                        // SSA copy: dest gets src's literal-status.
                        constant_text[dest.0 as usize] = constant_text[src.0 as usize].clone();
                    }
                    Rvalue::Call {
                        kind: CallKind::PrintlnConcat,
                        args,
                    } => {
                        let format = build_concat_format(func, &constant_text, args);
                        self.intern_concat_format(format);
                    }
                    _ => {}
                }
            }
        }
    }

    fn intern_concat_format(&mut self, format_text: String) -> String {
        if let Some(name) = self.concat_format_lookup.get(&format_text) {
            return name.clone();
        }
        let name = format!("@.fmt.concat.{}", self.concat_format_globals.len());
        self.concat_format_globals
            .push((name.clone(), format_text.clone()));
        self.concat_format_lookup.insert(format_text, name.clone());
        name
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
        // Split-borrow self into the four fields the walker needs;
        // this lets the walker hold a `&mut out` alongside a
        // `&concat_format_lookup` without the borrow checker
        // complaining about overlapping borrows of `self`.
        let module = self.module;
        let string_globals: &[String] = &self.string_globals;
        let concat_format_lookup = &self.concat_format_lookup;
        let out: &mut String = &mut self.out;
        let mut walker = BlockWalker {
            module,
            func,
            string_globals,
            concat_format_lookup,
            out,
            ssa_for_local: vec![None; func.locals.len()],
            constant_text: vec![None; func.locals.len()],
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
    /// `text → global name` lookup for `PrintlnConcat` format strings.
    /// Populated by `Emitter::scan_concat_formats` before any function
    /// body is walked.
    concat_format_lookup: &'a HashMap<String, String>,
    out: &'a mut String,
    /// For each MIR LocalId, the LLVM SSA value name that holds its
    /// current value (e.g. `%tmp3`, `%arg1`, or a literal). `None`
    /// means the local hasn't been materialized yet.
    ssa_for_local: Vec<Option<String>>,
    /// For each MIR LocalId, the literal text if it was assigned a
    /// constant string. Used by `lower_println_concat` to decide
    /// whether each arg should be inlined into the format string or
    /// referenced via `%s`/`%d`. Mirrors the same field used by
    /// `Emitter::scan_concat_formats` so the format string built at
    /// codegen time matches what was interned during the pre-scan.
    constant_text: Vec<Option<String>>,
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
                // Also propagate the literal-string status so a
                // chain like `val a = "hi"; val b = a;` keeps `b`
                // recognizable as a constant for `PrintlnConcat`
                // format-string folding.
                let s = self.ssa_of(*src);
                self.ssa_for_local[dest.0 as usize] = Some(s);
                self.constant_text[dest.0 as usize] = self.constant_text[src.0 as usize].clone();
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
                // We bind the local directly to the global pointer
                // *and* record the literal text so the
                // `PrintlnConcat` lowering can fold this constant
                // into the format string.
                let global = &self.string_globals[sid.0 as usize];
                self.ssa_for_local[dest.0 as usize] = Some(global.clone());
                self.constant_text[dest.0 as usize] =
                    Some(self.module.lookup_string(*sid).to_string());
            }
        }
    }

    fn lower_call_void(&mut self, rvalue: &Rvalue) {
        let Rvalue::Call { kind, args } = rvalue else {
            return;
        };
        match kind {
            CallKind::Println => self.lower_println(args),
            CallKind::PrintlnConcat => self.lower_println_concat(args),
            CallKind::Static(target_id) => self.lower_static_call(*target_id, args),
        }
    }

    /// Lower `CallKind::PrintlnConcat` to a single `printf` call.
    ///
    /// The format string was pre-computed and interned in the
    /// `Emitter`'s `concat_format_lookup` during the pre-scan, so
    /// we just rebuild it here (deterministically) and look up the
    /// matching global. The runtime args are everything that wasn't
    /// folded into the format string at pre-scan time.
    fn lower_println_concat(&mut self, args: &[LocalId]) {
        let format = build_concat_format(self.func, &self.constant_text, args);
        let fmt_global = self
            .concat_format_lookup
            .get(&format)
            .cloned()
            .expect("PrintlnConcat format string was not interned during pre-scan");

        // Collect runtime args (everything that wasn't a constant string).
        let mut runtime: Vec<String> = Vec::new();
        for &arg in args {
            if self.constant_text[arg.0 as usize].is_some() {
                continue; // inlined into the format string
            }
            let arg_ty = &self.func.locals[arg.0 as usize];
            let ssa = self.ssa_of(arg);
            let arg_text = match arg_ty {
                Ty::Int | Ty::Bool => format!("i32 {ssa}"),
                _ => format!("ptr {ssa}"),
            };
            runtime.push(arg_text);
        }

        let _ = self.fresh();
        write!(self.out, "  call i32 (ptr, ...) @printf(ptr {fmt_global}").unwrap();
        for arg_text in &runtime {
            write!(self.out, ", {arg_text}").unwrap();
        }
        writeln!(self.out, ")").unwrap();
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

/// Build the printf format string for a `PrintlnConcat` call.
///
/// `constant_text[i]` is `Some(text)` if MIR local `i` was assigned
/// a `Const(MirConst::String(_))` (or copied from one). Constant
/// args are inlined into the format string verbatim — with `%`
/// escaped to `%%` so printf doesn't reinterpret them. Runtime args
/// become `%s` or `%d` depending on type.
///
/// A trailing `\n` is appended because `println` (the kotlin builtin
/// we're lowering) always emits a newline.
///
/// This function is **deterministic and pure**: given the same MIR
/// function, constant_text, and args, it always returns the same
/// string. Both `Emitter::scan_concat_formats` (pre-scan) and
/// `BlockWalker::lower_println_concat` (codegen) call it, and they
/// agree by construction — the codegen path uses the result to look
/// up the global the pre-scan interned.
fn build_concat_format(
    func: &MirFunction,
    constant_text: &[Option<String>],
    args: &[LocalId],
) -> String {
    let mut format = String::new();
    for &arg in args {
        if let Some(literal) = &constant_text[arg.0 as usize] {
            for ch in literal.chars() {
                if ch == '%' {
                    format.push_str("%%");
                } else {
                    format.push(ch);
                }
            }
        } else {
            let arg_ty = &func.locals[arg.0 as usize];
            match arg_ty {
                Ty::Int | Ty::Bool => format.push_str("%d"),
                Ty::Long => format.push_str("%lld"),
                _ => format.push_str("%s"),
            }
        }
    }
    format.push('\n');
    format
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
    use skotch_intern::Interner;
    use skotch_lexer::lex;
    use skotch_mir_lower::lower_file;
    use skotch_parser::parse_file;
    use skotch_resolve::resolve_file;
    use skotch_span::FileId;
    use skotch_typeck::type_check;

    fn build(src: &str) -> String {
        let mut interner = Interner::new();
        let mut diags = skotch_diagnostics::Diagnostics::new();
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
        let mut diags = skotch_diagnostics::Diagnostics::new();
        let lf = lex(
            FileId(0),
            r#"fun main() { println("Hello, world!") }"#,
            &mut diags,
        );
        let ast = parse_file(&lf, &mut interner, &mut diags);
        let r = resolve_file(&ast, &mut interner, &mut diags);
        let t = type_check(&ast, &r, &mut interner, &mut diags);
        let m = lower_file(&ast, &r, &t, &mut interner, &mut diags, "InputKt");
        let klib =
            skotch_backend_klib::write_klib(&m, skotch_backend_klib::DEFAULT_TARGET).unwrap();
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
