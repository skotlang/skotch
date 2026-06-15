import java.util.function.LongFunction;
import java.util.concurrent.atomic.AtomicLong;
public class ArtLongCtorRef {
    static long make(LongFunction<AtomicLong> f, long x) { return f.apply(x).get(); }
    static long compute(long x) {
        LongFunction<AtomicLong> f = AtomicLong::new;
        return make(f, x);
    }
    public static void main(String[] z) {
        System.out.println(compute(42L));
        System.out.println(compute(-7L));
    }
}
