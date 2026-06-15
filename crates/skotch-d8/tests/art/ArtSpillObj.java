public class ArtSpillObj {
    String tag;
    ArtSpillObj(String t) { tag = t; }
    int compute(int n) {
        int a=n+1,b=n+2,c=n+3,d=n+4,e=n+5,g=n+6,h=n+7,i=n+8,j=n+9;
        int k=n+10,l=n+11,m=n+12,p=n+13,q=n+14,r=n+15,s=n+16,t=n+17;
        String z = this.tag;
        return a+b+c+d+e+g+h+i+j+k+l+m+p+q+r+s+t + z.length();
    }
    public static void main(String[] x) {
        System.out.println(new ArtSpillObj("hello").compute(0));
        System.out.println(new ArtSpillObj("hi").compute(5));
        System.out.println(new ArtSpillObj("abcdefg").compute(-2));
    }
}
