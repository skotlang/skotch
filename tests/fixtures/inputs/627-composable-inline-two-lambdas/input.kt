import androidx.compose.runtime.Composable

@Composable
inline fun Wrap(prefix: () -> Unit, suffix: () -> Unit) {
    prefix()
    suffix()
}
