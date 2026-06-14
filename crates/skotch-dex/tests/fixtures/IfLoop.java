public class IfLoop {
  static int condAdd(int n) { int s = 0; for (int i = 0; i < n; i++) { if (i > 2) s += i; } return s; }
  static int ifElse(int n) { int s = 0; for (int i = 0; i < n; i++) { if (i > 2) s += i; else s -= i; } return s; }
}
