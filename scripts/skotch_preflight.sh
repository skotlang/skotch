#!/bin/sh -ex
#cargo run -p xtask -- gen-fixtures --target klib
cargo test --workspace
cargo clippy --workspace --all-targets -- -Dwarnings
cargo fmt --all -- --check
