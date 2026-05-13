import androidx.compose.runtime.Composable

@Composable
inline fun Pred(check: Boolean, content: () -> Unit) {
    if (check) content()
}
