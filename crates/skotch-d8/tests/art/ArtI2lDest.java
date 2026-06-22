public class ArtI2lDest {
    static long compute(int n) {
        long a=1L,b=2L,c=3L,d=4L,e=5L,f=6L,g=7L;
        int x = n + 50;
        long r = (long) x;
        return r + a+b+c+d+e+f+g;
    }
    public static void main(String[] z){ System.out.println(compute(0)); System.out.println(compute(5)); }
}
