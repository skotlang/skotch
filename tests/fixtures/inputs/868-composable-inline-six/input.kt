import androidx.compose.runtime.Composable

@Composable
inline fun Six(a: Int, b: Long, c: Double, d: Float, e: Boolean, content: () -> Unit) {
    content()
}
