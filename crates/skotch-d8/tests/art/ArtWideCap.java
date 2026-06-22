import java.util.function.LongSupplier;
import java.util.function.DoubleSupplier;

// Capturing lambdas that capture a WIDE (long/double) value. The synthetic class gets a
// long/double field, the ctor takes the wide value and iput-wide's it, and the SAM method
// iget-wide's it and forwards it (both register halves) to the impl. Single class (built-in
// functional interfaces), deterministic output.
public class ArtWideCap {
    static LongSupplier longMul(long base) {
        return () -> base * 2;
    }

    static DoubleSupplier dblAdd(double base) {
        return () -> base + 0.5;
    }

    public static void main(String[] args) {
        System.out.println(longMul(100L).getAsLong());
        System.out.println(longMul(-5L).getAsLong());
        System.out.println(dblAdd(2.0).getAsDouble());
        System.out.println(dblAdd(-1.5).getAsDouble());
    }
}
