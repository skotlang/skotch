import androidx.compose.runtime.Composable

@Composable
fun Inner(a: Int = 0, b: Int = 0) {}

@Composable
fun Outer(content: @Composable () -> Unit) {
    content()
}

@Composable
fun Top() {
    Outer {
        Inner()
    }
}
