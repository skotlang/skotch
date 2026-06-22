import java.util.function.LongUnaryOperator;
public class ArtLongSam {
    static long apply(LongUnaryOperator op, long x) { return op.applyAsLong(x); }
    static long compute(int cap) {
        LongUnaryOperator op = (v) -> v + cap;
        return apply(op, 100L);
    }
    public static void main(String[] z) {
        System.out.println(compute(5));
        System.out.println(compute(-30));
    }
}
