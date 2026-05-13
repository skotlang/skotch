fun add(a: Int, b: Int): Int = a + b

suspend fun caller(x: Int): Int = add(x, 1)
