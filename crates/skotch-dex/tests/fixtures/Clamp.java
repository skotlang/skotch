public class Clamp {
  static int clampSum(int n) { int s = 0; for (int i = 0; i < n; i++) { int x = i; if (x > 5) x = 5; s += x; } return s; }
}
