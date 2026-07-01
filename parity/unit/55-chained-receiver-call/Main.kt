class C { fun double(x: Int) = x * 2 }
class B { val c = C() }
class A { val b = B() }

fun main() {
    val a = A()
    println(a.b.c.double(7))
}
