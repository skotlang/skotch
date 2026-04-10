annotation class Fancy(val value: String)

@Fancy("important")
class MyClass

fun main() {
    val ann = MyClass::class.annotations.first() as Fancy
    println(ann.value)
}
