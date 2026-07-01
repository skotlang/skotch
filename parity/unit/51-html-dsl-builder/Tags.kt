// Type-safe HTML builder DSL. The classic Kotlin DSL probe.
//
// Exercises:
//   - lambda-with-receiver as a constructor argument
//     (`init: HTML.() -> Unit`)
//   - nested receiver-typed lambdas (html { body { p { ... } } })
//   - `operator fun unaryPlus()` extension to add child text
//   - mutable child list + recursive render
//   - method-style child constructors (html.body { ... })

abstract class Tag(val name: String) {
    val children = mutableListOf<Tag>()
    val attrs = mutableMapOf<String, String>()

    open fun render(sb: StringBuilder, indent: String) {
        sb.append(indent).append("<").append(name)
        for ((k, v) in attrs) {
            sb.append(" ").append(k).append("=\"").append(v).append("\"")
        }
        if (children.isEmpty()) {
            sb.append("/>\n")
            return
        }
        sb.append(">\n")
        for (c in children) {
            c.render(sb, indent + "  ")
        }
        sb.append(indent).append("</").append(name).append(">\n")
    }
}

class TextNode(val text: String) : Tag("#text") {
    override fun render(sb: StringBuilder, indent: String) {
        sb.append(indent).append(text).append("\n")
    }
}

abstract class Container(name: String) : Tag(name) {
    operator fun String.unaryPlus() {
        children.add(TextNode(this))
    }
}

class HTML : Container("html") {
    fun body(init: Body.() -> Unit): Body {
        val b = Body()
        b.init()
        children.add(b)
        return b
    }
}

class Body : Container("body") {
    fun p(init: P.() -> Unit): P {
        val p = P()
        p.init()
        children.add(p)
        return p
    }

    fun h1(text: String): H1 {
        val h = H1()
        h.children.add(TextNode(text))
        children.add(h)
        return h
    }
}

class P : Container("p")
class H1 : Container("h1")

fun html(init: HTML.() -> Unit): HTML {
    val h = HTML()
    h.init()
    return h
}
