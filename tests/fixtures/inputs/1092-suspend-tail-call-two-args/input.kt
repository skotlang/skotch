suspend fun helper(a: Int, b: Int): Int = a + b
suspend fun caller(a: Int, b: Int): Int = helper(a, b)
