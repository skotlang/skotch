public class ArtHighArity {
    int fld;
    // this + 16 int params (na=17); s,t,r are live ACROSS the use of all args (the final sum),
    // so they can't reuse arg registers and land at v17+ — exercising the high-local nibble path.
    int big(int a,int b,int c,int d,int e,int f,int g,int h,
            int i,int j,int k,int l,int m,int n,int o,int p) {
        int s = a * 2 + b;
        int t = c * 3 + d;
        this.fld = s + t;                       // iput-int (22c): value high local
        int r;
        if (s > t) r = s - t; else r = t - s;   // if-gt (22t) on high locals s,t
        int u = this.fld;                       // iget-int (22c): dest high local u
        int[] arr = new int[(s & 3) + 1];       // new-array + array-length, size = high local
        // use ALL args at the end so a..p stay live across the above (forcing s,t,r,u high)
        int sum = a+b+c+d+e+f+g+h+i+j+k+l+m+n+o+p;
        return r + u + sum + arr.length + s + t;
    }
    public static void main(String[] x) {
        ArtHighArity z = new ArtHighArity();
        System.out.println(z.big(1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16));
        System.out.println(z.big(10,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1));
        System.out.println(z.big(0,5,9,2,1,1,1,1,1,1,1,1,1,1,1,1));
    }
}
