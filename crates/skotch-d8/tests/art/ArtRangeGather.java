public class ArtRangeGather {
    static int sinkBig(int a,int b,int c,int d,int e,int f,int g,
                       int h,int i,int j,int k,int l,int m,int o) {
        return a+b+c+d+e+f+g+h+i+j+k+l+m+o;
    }
    static long combine(int x, long y, String s) {
        return x + y + s.length();
    }
    static long compute(int n, long m, String tag) {
        int s = sinkBig(n,n,n,n,n,n,n,n,n,n,n,n,n,n);
        long r = combine(n, m, tag);
        if (s == 0) return -1L;
        return r;
    }
    public static void main(String[] z) {
        System.out.println(compute(2, 100L, "hello"));
        System.out.println(compute(3, 50L, "hi"));
        System.out.println(compute(5, 1000L, "abc"));
    }
}
