public class BreakLoop {
  static int withBreak(int n) { int s = 0; for (int i = 0; i < n; i++) { if (i > 5) break; s += i; } return s; }
  static int withContinue(int n) { int s = 0; for (int i = 0; i < n; i++) { if (i > 5) continue; s += i; } return s; }
}
