data class Address(val city: String, val zip: String)
data class Person2(val name: String, val addr: Address)

fun main() {
    val a = Address("ny", "10001")
    val p = Person2("alice", a)
    println(p)
    println(p.addr.city)
    println(p.addr.zip)
    val q = p.copy(addr = Address("la", "90001"))
    println(q)
}
