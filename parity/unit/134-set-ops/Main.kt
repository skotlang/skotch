fun main() {
    val a = setOf(1, 2, 3, 4)
    val b = setOf(3, 4, 5, 6)
    println(a.size)
    println(2 in a)
    println(5 in a)
    val u = (a + b).sorted()
    val i = a.intersect(b).sorted()
    val d = a.subtract(b).sorted()
    println(u)
    println(i)
    println(d)
}
