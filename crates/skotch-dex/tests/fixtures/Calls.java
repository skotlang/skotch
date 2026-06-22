public class Calls {
  static int twice(int x) { return x + x; }
  static int sumTwice(int n) { int s = 0; for (int i = 0; i < n; i++) s += twice(i); return s; }
}
