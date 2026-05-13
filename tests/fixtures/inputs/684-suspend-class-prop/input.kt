class Holder(val name: String)

suspend fun reveal(h: Holder): String = h.name
