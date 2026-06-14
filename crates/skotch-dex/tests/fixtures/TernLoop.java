public class TernLoop {
  static int pick(int n) { int s = 0; for (int i = 0; i < n; i++) { int x = (i > 0) ? 1 : 2; s += x; } return s; }
}
