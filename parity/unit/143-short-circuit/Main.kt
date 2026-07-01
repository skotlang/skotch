fun check(name: String, b: Boolean): Boolean {
    println("eval $name")
    return b
}

fun main() {
    println(check("a", true) && check("b", true))
    println(check("c", false) && check("d", true))
    println(check("e", true) || check("f", false))
    println(check("g", false) || check("h", true))
}
