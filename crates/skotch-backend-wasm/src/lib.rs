//! WebAssembly emitter — **stub for PR #1**.
//!
//! Real implementation arrives in PR #9. The plan is to use
//! `wasm-encoder` for section assembly and either WASI `fd_write`
//! or a JS host import for `println`.

use skotch_mir::MirModule;

/// Compile a [`MirModule`] to a WebAssembly module. **Not yet implemented.**
pub fn compile_module(_module: &MirModule) -> Vec<u8> {
    unimplemented!("WASM backend lands in PR #9");
}

#[cfg(test)]
mod tests {
    // The deliberate `unimplemented!()` is itself a reminder fixture.
    #[test]
    #[should_panic(expected = "WASM backend lands in PR #9")]
    fn backend_is_stubbed() {
        super::compile_module(&Default::default());
    }
}
