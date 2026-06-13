public class Calls2 {
  int k;
  Calls2(int k) { this.k = k; }
  int scale(int x) { return x * k; }
  static int triple(int x) { return x + x + x; }
  static void noop(int x) {}
  static int twoCalls(int n) { int s = 0; for (int i = 0; i < n; i++) { s += triple(i); noop(s); } return s; }
  static int viaInstance(Calls2 c, int n) { int s = 0; for (int i = 0; i < n; i++) s += c.scale(i); return s; }
}
