// Driver — builds a small HTML document and renders it.

fun renderDocument(doc: HTML): String {
    val sb = StringBuilder()
    doc.render(sb, "")
    return sb.toString()
}

fun main() {
    val doc = html {
        body {
            h1("hello")
            p {
                +"world"
                +"!"
            }
        }
    }

    val rendered = renderDocument(doc)
    println(rendered)
}
