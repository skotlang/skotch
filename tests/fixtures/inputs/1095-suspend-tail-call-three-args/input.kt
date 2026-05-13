suspend fun helper(a: Int, b: Int, c: Int): Int = a + b + c
suspend fun caller(a: Int, b: Int, c: Int): Int = helper(a, b, c)
