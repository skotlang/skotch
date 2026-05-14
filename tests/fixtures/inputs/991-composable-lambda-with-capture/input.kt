import androidx.compose.runtime.Composable

class Holder(val n: Int)

@Composable
fun Inner(a: Holder, b: Int = 0, c: Int = 0) {}

@Composable
fun Outer(content: @Composable () -> Unit) {
    content()
}

@Composable
fun Top(h: Holder) {
    Outer {
        Inner(h)
    }
}
