public class WideLoop {
  static long sumLongs(long[] a, int n) { long s = 0L; for (int i = 0; i < n; i++) s += a[i]; return s; }
  static long countUp(int n) { long s = 0L; for (int i = 0; i < n; i++) s += i; return s; }
  static double scale(double[] a, int n) { double s = 0.0; for (int i = 0; i < n; i++) s += a[i]; return s; }
}
