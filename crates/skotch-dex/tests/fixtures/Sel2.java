public class Sel2 {
  static int two(int a, int b, int c, int d, int n) {
    int s = 0;
    for (int i = 0; i < n; i++) {
      int x, y;
      if (i > 0) { x = a; y = c; } else { x = b; y = d; }
      s += x + y;
    }
    return s;
  }
}
