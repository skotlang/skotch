interface Printable {
    fun prettyPrint(): String
}

class Document(val title: String) : Printable {
    override fun prettyPrint(): String = "Document: $title"
}

fun main() {
    val doc: Printable = Document("Hello")
    println(doc.prettyPrint())
}
