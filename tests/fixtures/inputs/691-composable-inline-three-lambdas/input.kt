import androidx.compose.runtime.Composable

@Composable
inline fun Sandwich(top: () -> Unit, middle: () -> Unit, bottom: () -> Unit) {
    top()
    middle()
    bottom()
}
