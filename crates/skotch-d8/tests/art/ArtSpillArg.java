public class ArtSpillArg {
    int base;
    ArtSpillArg(int b) { base = b; }
    static int sink(int a,int b,int c,int d,int e,int f,int g,int h,
                    int i,int j,int k,int l,int m,int o,int p,int q) {
        return a+b+c+d+e+f+g+h+i+j+k+l+m+o+p+q;
    }
    int compute(int n) {
        int s = sink(n,n,n,n,n,n,n,n,n,n,n,n,n,n,n,n);
        int old = this.base;
        this.base = s;
        return old;
    }
    public static void main(String[] z) {
        System.out.println(new ArtSpillArg(1000).compute(0));
        System.out.println(new ArtSpillArg(50).compute(5));
        System.out.println(new ArtSpillArg(-7).compute(-2));
    }
}
