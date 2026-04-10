class Outer {
    class Nested {
        fun message(): String = "I am nested"
    }
}

fun main() {
    println(Outer.Nested().message())
}
