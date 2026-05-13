import androidx.compose.runtime.Composable

@Composable
fun Inner() {}

@Composable
fun Outer() {
    Inner()
}
