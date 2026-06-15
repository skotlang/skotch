public class ArtSwitchWide {
    static int sum13(int a,int b,int c,int d,int e,int f,int g,
                     int h,int i,int j,int k,int l,int m) {
        return a+b+c+d+e+f+g+h+i+j+k+l+m;
    }
    static int compute(int n, int sel) {
        int a=n+1,b=n+2,c=n+3,d=n+4,e=n+5,f=n+6,g=n+7,h=n+8,i=n+9,j=n+10,k=n+11,l=n+12,m=n+13;
        int s = sum13(a,b,c,d,e,f,g,h,i,j,k,l,m);
        if (s < 0) return -2;
        switch (sel) {
            case 1: return 100;
            case 5: return 500;
            case 9: return 900;
            default: return -1;
        }
    }
    public static void main(String[] z) {
        System.out.println(compute(2, 1));
        System.out.println(compute(2, 5));
        System.out.println(compute(2, 9));
        System.out.println(compute(2, 7));
    }
}
