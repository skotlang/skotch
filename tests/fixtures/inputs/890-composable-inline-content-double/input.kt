import androidx.compose.runtime.Composable

@Composable
inline fun Sized(width: Double, height: Double, content: () -> Unit) {
    content()
}
