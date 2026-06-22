public class DivLoop {
  static int sumDiv(int n) { int s = 0; for (int i = 1; i < n; i++) s += n / i; return s; }
  static int halves(int n) { int s = 0; for (int i = 0; i < n; i++) s += i / 2; return s; }
  static int mods(int n) { int s = 0; for (int i = 1; i < n; i++) s += n % i; return s; }
}
