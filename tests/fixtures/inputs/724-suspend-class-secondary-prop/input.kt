class P(val a: Int, val b: String)

suspend fun getA(p: P): Int = p.a
suspend fun getB(p: P): String = p.b
