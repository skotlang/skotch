// TODO: named arguments at the call site.
fun box(width: Int, height: Int) {
    println(width * height)
}

fun main() {
    box(height = 3, width = 4)
}
