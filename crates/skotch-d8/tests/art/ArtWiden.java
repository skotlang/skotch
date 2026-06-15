import java.util.function.*;
public class ArtWiden {
    static double dacc = 0;
    static long   lacc = 0;
    static void idImpl(double x) { dacc += x * 1.5; }     // int->double (IntConsumer)
    static void ilImpl(long x)   { lacc += x * 3; }       // int->long   (IntConsumer)
    static void ldImpl(double x) { dacc += x / 4.0; }     // long->double (LongConsumer)
    static void runI(IntConsumer c, int v)   { c.accept(v); }
    static void runL(LongConsumer c, long v) { c.accept(v); }
    public static void main(String[] z) {
        runI(ArtWiden::idImpl, 21);
        runI(ArtWiden::idImpl, 100);
        runI(ArtWiden::ilImpl, 7);
        runL(ArtWiden::ldImpl, 40L);
        System.out.println(dacc);
        System.out.println(lacc);
    }
}
