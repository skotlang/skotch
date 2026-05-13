import androidx.compose.runtime.Composable

@Composable
inline fun Multi(a: Int, b: Long, c: Double, d: Float, e: Boolean, content: () -> Unit) {
    content()
}
