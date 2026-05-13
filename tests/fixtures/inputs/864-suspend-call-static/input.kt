object Const { fun get(): Int = 100 }

suspend fun bar(): Int = Const.get()
