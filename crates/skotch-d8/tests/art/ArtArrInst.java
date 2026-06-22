public class ArtArrInst {
    static int sum13(int a,int b,int c,int d,int e,int f,int g,
                     int h,int i,int j,int k,int l,int m) {
        return a+b+c+d+e+f+g+h+i+j+k+l+m;
    }
    static int compute(int[] arr, Object obj, int n) {
        int a=n+1,b=n+2,c=n+3,d=n+4,e=n+5,f=n+6,g=n+7,h=n+8,i=n+9,j=n+10,k=n+11,l=n+12,m=n+13;
        int s = sum13(a,b,c,d,e,f,g,h,i,j,k,l,m);
        int len = arr.length;
        int isStr = (obj instanceof String) ? 1 : 0;
        if (s < 0) return -1;
        return len * 10 + isStr;
    }
    public static void main(String[] z) {
        System.out.println(compute(new int[]{1,2,3}, "hi", 2));
        System.out.println(compute(new int[]{}, Integer.valueOf(5), 0));
        System.out.println(compute(new int[]{1,2,3,4,5}, "x", 1));
    }
}
