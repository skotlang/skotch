val x = 42
val label = when {
    x < 0 -> "negative"
    x == 0 -> "zero"
    else -> "positive"
}
println(label)
