// User class implementing `Comparable<T>` with a specialized
// `compareTo(T)` method. Pre-fix: the synthesized class lacked
// the `compareTo(Object)` bridge that java/lang/Comparable's
// abstract method dispatches through. Calls via the Comparable
// interface (e.g. `CollectionsKt.sorted` → `TimSort` →
// `((Comparable)o).compareTo(other)`) threw `AbstractMethodError:
// Receiver class Person does not define or inherit an
// implementation of the resolved method 'abstract int
// compareTo(java.lang.Object)' of interface java.lang.Comparable`.
//
// Fix at mir-lower:~25898 detects classes implementing
// java/lang/Comparable with a typed `compareTo(X)` override and
// synthesizes the `compareTo(Object)` bridge:
//   public int compareTo(Object o) {
//       return compareTo((Person) o);
//   }

class Person(val name: String, val age: Int) : Comparable<Person> {
    override fun compareTo(other: Person): Int = age - other.age
    override fun toString(): String = "${name}(${age})"
}

fun main() {
    val people = listOf(
        Person("Carol", 35),
        Person("Alice", 30),
        Person("Bob", 25)
    )
    for (p in people.sorted()) {
        println(p)
    }
}
