// The opt-in marker annotations live in a NESTED subpackage of the
// library package, exactly like kotlin-result's
// com.github.michaelbull.result.annotation.UnsafeResultValueAccess.
package com.example.result.annotation

@RequiresOptIn(
    message = "This is an unsafe value accessor.",
    level = RequiresOptIn.Level.ERROR,
)
@Retention(AnnotationRetention.BINARY)
@Target(AnnotationTarget.PROPERTY)
annotation class UnsafeBoxValueAccess

@RequiresOptIn(
    message = "This is an unsafe error accessor.",
    level = RequiresOptIn.Level.ERROR,
)
@Retention(AnnotationRetention.BINARY)
@Target(AnnotationTarget.PROPERTY)
annotation class UnsafeBoxErrorAccess
