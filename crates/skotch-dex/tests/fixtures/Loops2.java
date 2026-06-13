public class Loops2 {
  // downward loop: counts down from n, different comparison (i > 0) and decrement.
  static int down(int n) { int s = 0; for (int i = n; i > 0; i--) s += i; return s; }
  // running product with <= comparison and a multiply in the body.
  static int fact(int n) { int p = 1; for (int i = 1; i <= n; i++) p *= i; return p; }
  // nested loop: three live loop variables (outer i, inner j, accumulator t).
  static int grid(int n) { int t = 0; for (int i = 0; i < n; i++) for (int j = 0; j < n; j++) t += i * j; return t; }
}
