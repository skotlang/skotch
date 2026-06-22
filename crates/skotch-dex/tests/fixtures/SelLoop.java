public class SelLoop {
  static int sel(int a, int b, int n) { int s = 0; for (int i = 0; i < n; i++) { int x = (i > 5) ? a : b; s += x; } return s; }
}
