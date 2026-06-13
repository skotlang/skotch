public class Br {
  public static int sign(int x){ if(x>0) return 1; if(x<0) return -1; return 0; }
  public static int absv(int x){ if(x<0) return -x; return x; }
  public static int max2(int a,int b){ if(a>=b) return a; return b; }
  public static int clamp0(int x){ if(x<0) return 0; return x; }
}
