public class NewLoop {
  static int[] iota(int n) { int[] a = new int[n]; for (int i = 0; i < n; i++) a[i] = i; return a; }
  static int build(int n) { StringBuilder sb = new StringBuilder(); for (int i = 0; i < n; i++) sb.append(i); return sb.length(); }
}
