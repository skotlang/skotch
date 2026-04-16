class Container(val name: String) {
    class Item(val value: Int)
}
fun main() {
    val item = Container.Item(42)
    println(item.value)
}
