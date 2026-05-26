// A plain (non-val/var) constructor parameter has no backing field — it is
// an ordinary local used for super delegation or to initialize body
// properties. Emitting a `putfield` for it in `<init>` (or reading it as
// `this.<param>`) targets a nonexistent field → NoSuchFieldError at
// construction time. Only `val`/`var` params get fields. Mirrors JetChat's
// `ConversationUiState(channelName, channelMembers, initialMessages:
// List<Message>)`, where `initialMessages` is a plain param feeding
// `_messages = initialMessages.toMutableStateList()`.
class Box(val name: String, contents: List<String>) {
    val items: List<String> = contents
}

fun main() {
    val b = Box("hi", listOf("a", "b", "c"))
    println(b.name)
    println(b.items.size)
}
