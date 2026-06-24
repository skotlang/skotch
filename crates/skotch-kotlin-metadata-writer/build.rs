//! Compile the vendored Kotlin metadata `.proto` files into Rust types
//! via `prost-build`. Generated code lands in `OUT_DIR` and is pulled
//! into `lib.rs` with `include!`. The vendored copies (pinned to
//! Kotlin v2.4.0) live under `proto/` and retain their original
//! `core/metadata{,.jvm}/src/...` directory layout so the
//! cross-file `import` lines in the upstream files resolve unmodified.

fn main() {
    // Use the vendored protoc binary so the build does not require a
    // system `protoc` install (CI runners typically don't have one).
    let protoc = protoc_bin_vendored::protoc_bin_path()
        .expect("protoc-bin-vendored: no protoc binary for this host platform");
    // SAFETY: build scripts run single-threaded before any user code.
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    let proto_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("proto");

    // Tell cargo to rerun the build script when any vendored proto
    // (or this script) changes.
    println!("cargo:rerun-if-changed=build.rs");
    for sub in [
        "core/metadata/src/metadata.proto",
        "core/metadata/src/ext_options.proto",
        "core/metadata.jvm/src/jvm_metadata.proto",
        "core/metadata.jvm/src/jvm_module.proto",
    ] {
        println!("cargo:rerun-if-changed=proto/{sub}", sub = sub,);
    }

    let inputs = [
        "core/metadata/src/metadata.proto",
        "core/metadata.jvm/src/jvm_metadata.proto",
        "core/metadata.jvm/src/jvm_module.proto",
    ]
    .iter()
    .map(|p| proto_root.join(p))
    .collect::<Vec<_>>();

    prost_build::Config::new()
        // We only need the message types — Kotlin's custom field/message
        // options (`(string_id_in_table) = true`, ...) are descriptor
        // metadata; prost-build accepts them via descriptor.proto but
        // does not surface them in the generated Rust.
        .compile_protos(&inputs, &[proto_root])
        .expect("prost-build: compile vendored Kotlin metadata protos");
}
