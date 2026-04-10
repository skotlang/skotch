class MyClass {
    companion object {
        fun create(): MyClass = MyClass()
        const val TAG = "MyClass"
    }
}

fun main() {
    val obj = MyClass.create()
    println(MyClass.TAG)
    println(obj)
}
