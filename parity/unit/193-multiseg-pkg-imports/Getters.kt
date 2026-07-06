// Extension functions in a separate file of the same multi-segment
// package — Main.kt imports these BY NAME (`import
// com.example.result.getOr`), which is the exact shape of 102-result's
// `import com.github.michaelbull.result.andThen` / `.getOr` imports
// that come back "unresolved reference".
package com.example.result

import com.example.result.annotation.UnsafeBoxErrorAccess
import com.example.result.annotation.UnsafeBoxValueAccess

@OptIn(UnsafeBoxValueAccess::class)
fun <V, E> Box<V, E>.getOr(default: V): V {
    return if (isOk) value else default
}

@OptIn(UnsafeBoxErrorAccess::class)
fun <V, E> Box<V, E>.getErrorOr(default: E): E {
    return if (isErr) error else default
}
