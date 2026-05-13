import androidx.compose.runtime.Composable

@Composable
inline fun <T> Provide(value: T, content: () -> Unit) {
    content()
}
