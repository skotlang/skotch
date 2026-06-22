public class ArtCtorRange {
    final int sum;
    public ArtCtorRange(int a, int b, int c, int d, int e, int f) { sum = a + b*2 + c*3 + d*4 + e*5 + f*6; }
    interface Mk { ArtCtorRange make(int a, int b, int c, int d, int e, int f); }
    static int run(Mk m) { return m.make(1, 2, 3, 4, 5, 6).sum; }
    static int run2(Mk m) { return m.make(10, 20, 30, 40, 50, 60).sum; }
    public static void main(String[] x) {
        System.out.println(run(ArtCtorRange::new));
        System.out.println(run2(ArtCtorRange::new));
    }
}
