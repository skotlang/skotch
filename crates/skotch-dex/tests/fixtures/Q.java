public class Q {
  int n;
  int getN(){ return n; }
  static int viaStatic(Q q){ return q.getN(); }
  int addN(int k){ return n + k; }
}
