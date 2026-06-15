public class ArtPhiWideMove {
    static int sum13(int a,int b,int c,int d,int e,int f,int g,
                     int h,int i,int j,int k,int l,int m) {
        return a+b+c+d+e+f+g+h+i+j+k+l+m;
    }
    static int compute(int n, int seed) {
        int a=n+1,b=n+2,c=n+3,d=n+4,e=n+5,f=n+6,g=n+7,h=n+8,i=n+9,j=n+10,k=n+11,l=n+12,m=n+13;
        int s = sum13(a,b,c,d,e,f,g,h,i,j,k,l,m);
        int acc = seed;
        int z = n;
        while (z != 0) { acc = acc + 1; z = z - 1; }
        if (s < 0) return -1;
        return acc;
    }
    public static void main(String[] w) {
        System.out.println(compute(2, 100));
        System.out.println(compute(3, 50));
        System.out.println(compute(0, 7));
    }
}
