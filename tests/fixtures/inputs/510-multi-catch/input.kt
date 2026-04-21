fun main() {
    try {
        throw IllegalArgumentException("test")
    } catch (e: IllegalStateException) {
        println("caught ISE")
    } catch (e: IllegalArgumentException) {
        println("caught IAE: ${e.message}")
    }
}
