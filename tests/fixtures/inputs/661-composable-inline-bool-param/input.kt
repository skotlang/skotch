import androidx.compose.runtime.Composable

@Composable
inline fun Cond(flag: Boolean, content: () -> Unit) {
    if (flag) content()
}
