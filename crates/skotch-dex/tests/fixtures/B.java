public class B {
  public static int id(int a){ return a; }
  public static int add(int a,int b){ return a+b; }
  public static int addc(int a){ return a+1; }
  public static int sub(int a,int b){ return a-b; }
  public static int mul(int a,int b){ return a*b; }
  public static int constants(){ return 5; }
  public static int bigconst(){ return 100000; }
  public static long lconst(){ return 7L; }
  public static int field;
  public static int getf(){ return field; }
  public static void setf(int v){ field=v; }
  public static void vcall(){ System.out.println("hi"); }
}
