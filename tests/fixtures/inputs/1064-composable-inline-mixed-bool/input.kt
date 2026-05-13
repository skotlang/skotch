import androidx.compose.runtime.Composable

@Composable
inline fun M(a: Boolean, b: Int, c: String, content: () -> Unit) {
    if (a) content()
}
