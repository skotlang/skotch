public class S {
  public static int id(int a){ return a; }
  public static int add(int a,int b){ return a+b; }
  public static int addc(int a){ return a+1; }
  public static int two(int a){ int t=a*2; return t+1; }
  public static int constants(){ return 5; }
  public static int bigconst(){ return 100000; }
  public static long lconst(){ return 7L; }
  public static void vcall(){ System.out.println("hi"); }
  public static int field; 
  public static int getf(){ return field; }
  public static void setf(int v){ field=v; }
}
