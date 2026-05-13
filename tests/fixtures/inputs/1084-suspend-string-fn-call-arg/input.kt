fun greet(s: String): String = "Hi $s"
suspend fun fwd(s: String): String = greet(s)
