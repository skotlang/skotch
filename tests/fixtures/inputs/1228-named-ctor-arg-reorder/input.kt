// Named constructor arguments must be reordered to the constructor's
// declared parameter order before emission. `Holder(items = …, name = …,
// count = …)` must emit `<init>(name, count, items)` — kotlinc always
// uses the callee's positional descriptor. Without the reorder the args
// emit in source order, mismatching the `<init>` signature (VerifyError
// same-file / NoSuchMethodError cross-file). Mirrors JetChat's launch
// path `ConversationUiState(initialMessages = …, channelName = …,
// channelMembers = …)`. The List argument also comes from a top-level
// `val` (getstatic) so this exercises a real reordered reference arg.
class Holder(val name: String, val count: Int, val items: List<String>)

val theItems = listOf("x", "y", "z")
val theHolder = Holder(items = theItems, name = "hi", count = 42)

fun main() {
    println(theHolder.name)
    println(theHolder.count)
    println(theHolder.items.size)
}
