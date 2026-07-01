fun main() {
    val counter = ObservedCounter()
    counter.addObserver(Logger("primary"))
    counter.addObserver(Logger("audit"))
    counter.addObserver(ThresholdAlert(3))

    counter.increment()
    counter.increment()
    counter.incrementBy(2)
    counter.increment()

    println("final count: ${counter.count}")
}
