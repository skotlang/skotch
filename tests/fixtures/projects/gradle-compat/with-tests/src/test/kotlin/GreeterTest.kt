import org.junit.jupiter.api.Test

class GreeterTest {
    @Test
    fun testGreet() {
        val result = greet("World")
        if (result != "Hello, World!") {
            throw AssertionError("Expected 'Hello, World!' but got '$result'")
        }
    }

    @Test
    fun testAdd() {
        val result = add(2, 3)
        if (result != 5) {
            throw AssertionError("Expected 5 but got $result")
        }
    }

    @Test
    fun testGreetEmpty() {
        val result = greet("")
        if (result != "Hello, !") {
            throw AssertionError("Expected 'Hello, !' but got '$result'")
        }
    }
}
