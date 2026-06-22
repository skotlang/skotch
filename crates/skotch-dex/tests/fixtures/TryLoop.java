public class TryLoop {
  static int sumSafe(int n) {
    int s = 0;
    for (int i = 0; i < n; i++) {
      try { s += n / i; } catch (ArithmeticException e) { s += 1; }
    }
    return s;
  }
}
