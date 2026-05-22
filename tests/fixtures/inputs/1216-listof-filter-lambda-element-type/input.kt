// Locks in: `listOf(Message(...))` infers `List<Message>` (Ty::Generic);
// then `.filter { it.author != "me" }` propagates Message to the
// lambda's implicit `it` so `it.author` resolves correctly.
//
// Without inference: `it` is `Ty::Any`, `it.author` fails to resolve,
// the lambda body gets silently dropped → ClassCastException at
// runtime when the filter passes a Message into a String slot.

data class Message(val author: String, val text: String)

fun main() {
    val msgs = listOf(
        Message("me", "Hello"),
        Message("you", "Hi back"),
        Message("me", "How are you?"),
    )
    val mine = msgs.filter { it.author == "me" }
    println(mine.size)
}
