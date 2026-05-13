class Box(val n: Int)

suspend fun get(b: Box): Int = b.n
