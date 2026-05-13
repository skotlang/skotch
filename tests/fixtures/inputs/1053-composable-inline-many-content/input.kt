import androidx.compose.runtime.Composable

@Composable
inline fun M(c1: () -> Unit, c2: () -> Unit, c3: () -> Unit, c4: () -> Unit, c5: () -> Unit) {
    c1()
    c2()
    c3()
    c4()
    c5()
}
