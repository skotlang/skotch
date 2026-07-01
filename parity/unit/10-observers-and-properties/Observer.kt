// Interface with a single abstract method — implementors react to
// state changes broadcast from the ObservedCounter below.
interface Observer {
    fun onChange(value: Int)
}

// Logger that prints a tagged message on each notification.
class Logger(val name: String) : Observer {
    override fun onChange(value: Int) {
        println("[$name] count is now $value")
    }
}

// A second observer kind that only fires once per threshold cross.
class ThresholdAlert(val threshold: Int) : Observer {
    private var triggered: Boolean = false

    override fun onChange(value: Int) {
        if (!triggered && value >= threshold) {
            triggered = true
            println("ALERT: crossed threshold $threshold")
        }
    }
}
