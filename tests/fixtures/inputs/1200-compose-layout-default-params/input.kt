package test

class Receiver {
    fun layout(width: Int, height: Int, alignmentLines: String = "", placementBlock: () -> Unit): String {
        return "$width:$height"
    }
}

fun useReceiver(r: Receiver): String {
    return r.layout(100, 50) {
        // placement block (body intentionally empty)
    }
}

fun main() {
    println(useReceiver(Receiver()))
}
