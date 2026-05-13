import androidx.compose.runtime.Composable

@Composable
inline fun WithCount(count: Int, content: () -> Unit) {
    content()
}
