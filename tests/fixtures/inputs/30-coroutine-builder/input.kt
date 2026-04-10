// TODO: coroutine builders (`runBlocking`, `launch`). Depends on the
// kotlinx.coroutines runtime so this is more about correctly resolving
// the imported builder than emitting any new bytecode shape.
import kotlinx.coroutines.runBlocking

fun main() = runBlocking {
    println("hi from runBlocking")
}
