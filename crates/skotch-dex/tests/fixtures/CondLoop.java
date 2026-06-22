public class CondLoop {
  static int andCond(int n) { int s = 0; for (int i = 0; i < n; i++) { if (i > 2 && i < 8) s += i; } return s; }
  static int orCond(int n) { int s = 0; for (int i = 0; i < n; i++) { if (i < 2 || i > 8) s += i; } return s; }
  static int nestedIf(int n) { int s = 0; for (int i = 0; i < n; i++) { if (i > 2) { if (i < 8) s += i; } } return s; }
}
