#!/bin/sh -ex
#cargo run -p xtask -- gen-fixtures --target klib
cargo clippy --workspace --all-targets -- -Dwarnings
cargo fmt --all -- --check
cargo test --workspace
