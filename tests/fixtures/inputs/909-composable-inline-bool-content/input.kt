import androidx.compose.runtime.Composable

@Composable
inline fun WithFlag(flag: Boolean, content: () -> Unit) {
    if (flag) content()
}
