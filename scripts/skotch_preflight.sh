#!/bin/sh -ex
#cargo run -p xtask -- gen-fixtures --target klib
#cargo fmt --all -- --check
cargo fmt --all
cargo clippy --workspace --all-targets -- -Dwarnings
cargo test --workspace
