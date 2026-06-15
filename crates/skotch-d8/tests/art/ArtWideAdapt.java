import java.util.function.Function;
public class ArtWideAdapt {
    static long   limpl(long x)   { return x * 2 + 1; }
    static String simpl(long x)   { return "L" + (x + 5); }
    static double dimpl(double x) { return x / 2.0 + 0.25; }
    static long   la(Function<Long, Long> f, long v)       { return f.apply(v); }
    static String sa(Function<Long, String> f, long v)     { return f.apply(v); }
    static double da(Function<Double, Double> f, double v) { return f.apply(v); }
    public static void main(String[] a) {
        System.out.println(la(ArtWideAdapt::limpl, 21L));    // long param-unbox + long return-box
        System.out.println(la(ArtWideAdapt::limpl, 100L));
        System.out.println(sa(ArtWideAdapt::simpl, 100L));   // long param-unbox, String return
        System.out.println(da(ArtWideAdapt::dimpl, 9.0));    // double param-unbox + double return-box
    }
}
