// User class implementing `Comparable<Person>`. Kotlin's bytecode
// requirements:
//   - `public int compareTo(Person other)` — the user method.
//   - `public int compareTo(java.lang.Object o)` — the synthetic
//     bridge that checkcasts Object → Person and forwards.
//
// java/lang/Comparable's abstract method is `compareTo(Object)`
// (erased generic). Without the bridge, calls through the
// Comparable interface (e.g. `Collections.sort` →
// `((Comparable)o).compareTo(other)`) throw `AbstractMethodError`.

class Person(val name: String, val age: Int) : Comparable<Person> {
    override fun compareTo(other: Person): Int = age - other.age
    override fun toString(): String = "${name}(${age})"
}
