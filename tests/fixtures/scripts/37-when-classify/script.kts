val x = 42
val result = when {
    x < 0 -> "negative"
    x == 0 -> "zero"
    x < 10 -> "small"
    else -> "large"
}
println(result)
