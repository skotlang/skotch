public class ArrLoop {
  static int sumArr(int[] a, int n) { int s = 0; for (int i = 0; i < n; i++) s += a[i]; return s; }
  static void fillSquares(int[] a, int n) { for (int i = 0; i < n; i++) a[i] = i * i; }
  static int total(int[] a) { int s = 0; for (int i = 0; i < a.length; i++) s += a[i]; return s; }
}
