// Counter with private mutable state + a public-read property
// exposed via a custom getter, an Observer list, and a private
// `notifyAll` helper. Exercises:
//   - `private var` backing field
//   - `val ... get() = ...` custom property accessor
//   - polymorphic dispatch over a mutable list of interface types
//   - private member function called from within the same class
class ObservedCounter {
    private var _count: Int = 0
    private val observers: MutableList<Observer> = mutableListOf()

    val count: Int
        get() = _count

    fun addObserver(o: Observer) {
        observers.add(o)
    }

    fun increment() {
        _count += 1
        broadcast()
    }

    fun incrementBy(n: Int) {
        _count += n
        broadcast()
    }

    private fun broadcast() {
        var i = 0
        while (i < observers.size) {
            observers[i].onChange(_count)
            i++
        }
    }
}
