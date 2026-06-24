# Vendored Kotlin metadata `.proto` schemas

This directory holds verbatim copies of the protobuf schemas that
describe the payload of the `@kotlin.Metadata` annotation. They are
the source-of-truth for what kotlinc emits and what
`skotch-classinfo::kotlin_metadata` reads back.

## Provenance

All files are taken unmodified (including their original directory
layout) from the JetBrains/kotlin repository, pinned to a single tag:

| File                                              | Upstream path                                           |
| ------------------------------------------------- | ------------------------------------------------------- |
| `core/metadata/src/metadata.proto`                | `core/metadata/src/metadata.proto`                      |
| `core/metadata/src/ext_options.proto`             | `core/metadata/src/ext_options.proto`                   |
| `core/metadata.jvm/src/jvm_metadata.proto`        | `core/metadata.jvm/src/jvm_metadata.proto`              |
| `core/metadata.jvm/src/jvm_module.proto`          | `core/metadata.jvm/src/jvm_module.proto`                |

Tag: **`v2.4.0`** (https://github.com/JetBrains/kotlin/tree/v2.4.0).
Fetch command (for re-syncing on a future bump):

```sh
TAG=v2.4.0
BASE=https://raw.githubusercontent.com/JetBrains/kotlin/$TAG
curl -sSLo core/metadata/src/metadata.proto         $BASE/core/metadata/src/metadata.proto
curl -sSLo core/metadata/src/ext_options.proto      $BASE/core/metadata/src/ext_options.proto
curl -sSLo core/metadata.jvm/src/jvm_metadata.proto $BASE/core/metadata.jvm/src/jvm_metadata.proto
curl -sSLo core/metadata.jvm/src/jvm_module.proto   $BASE/core/metadata.jvm/src/jvm_module.proto
```

The directory layout is preserved exactly so the upstream
`import "core/metadata/src/...";` lines resolve without any local
patching. `build.rs` passes this directory as the single `protoc`
include path.

## License

Apache-2.0 (same as the upstream Kotlin repository — see headers in
each file). These vendored copies are tooling inputs, not derivative
works under skotch's AGPL license.
