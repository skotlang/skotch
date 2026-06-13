public class Press2 {
  static int f4(int a, int b, int c, int d){ return (a + b) + (c + d); }
  static int chain(int a){ return ((a + 1) * 3 - 2) | 4; }
  static int fieldArith(int a){ return a * 1000 + 7; }
  static int twoConst(int a){ return (a | 1000000) + (a & 2000000); }
}
