import androidx.compose.runtime.Composable

@Composable
inline fun Before(pre: () -> Unit) {
    pre()
}
