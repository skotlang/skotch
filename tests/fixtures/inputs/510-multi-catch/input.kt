fun main() {
    try {
        throw IllegalStateException("test")
    } catch (e: IllegalStateException) {
        println("caught ISE")
    } catch (e: IllegalArgumentException) {
        println("caught IAE")
    }
}
