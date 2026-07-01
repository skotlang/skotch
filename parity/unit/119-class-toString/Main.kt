class Person(val name: String, val age: Int) {
    override fun toString(): String = "Person[$name,$age]"
    override fun equals(other: Any?): Boolean = other is Person && other.name == name && other.age == age
    override fun hashCode(): Int = name.hashCode() * 31 + age
}

fun main() {
    val a = Person("alice", 30)
    val b = Person("alice", 30)
    val c = Person("bob", 25)
    println(a)
    println(a == b)
    println(a == c)
    println(a.hashCode() == b.hashCode())
}
