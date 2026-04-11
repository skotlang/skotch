val x = 42
val size = when (x) {
    0 -> "zero"
    1 -> "one"
    else -> "many"
}
println(size)
