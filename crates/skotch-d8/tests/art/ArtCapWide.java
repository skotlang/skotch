import java.util.function.BiConsumer;
public class ArtCapWide {
    long total = 0;
    void add(Object key, long val) { total += val + (key.hashCode() & 7); }
    BiConsumer<Object, Long> consumer() { return this::add; }
    public static void main(String[] a) {
        ArtCapWide z = new ArtCapWide();
        BiConsumer<Object, Long> c = z.consumer();
        c.accept("k", 21L);
        c.accept("m", 100L);
        c.accept("zz", 1000L);
        System.out.println(z.total);
    }
}
