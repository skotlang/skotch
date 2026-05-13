import androidx.compose.runtime.Composable

@Composable
inline fun PairWrap(left: () -> Unit, right: () -> Unit) {
    left()
    right()
}
