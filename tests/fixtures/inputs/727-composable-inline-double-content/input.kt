import androidx.compose.runtime.Composable

@Composable
inline fun Sized(width: Double, content: () -> Unit) {
    content()
}
