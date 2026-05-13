class Box(val n: Int)

suspend fun extract(b: Box): Int = b.n
