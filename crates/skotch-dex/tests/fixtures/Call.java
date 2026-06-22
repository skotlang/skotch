public class Call {
  static int g(){ return 7; }
  static int h(int x){ return x; }
  public static int callNoArg(){ return g(); }
  public static int callChain(){ return g() + g(); }
  public static int viaH(){ return h(5); }
}
