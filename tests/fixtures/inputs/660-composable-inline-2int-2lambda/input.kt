import androidx.compose.runtime.Composable

@Composable
inline fun Wrap(min: Int, max: Int, pre: () -> Unit, post: () -> Unit) {
    pre()
    post()
}
