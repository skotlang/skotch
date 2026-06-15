// >16-register straight-line arithmetic. 18 locals + the parameter are all live
// simultaneously at the final sum, forcing the SSA allocator past 16 registers. The
// adds materialize as `add-int/lit8` (const fold) and `add-int` (3-address, 8-bit
// fields) — the 3-address form is what `emit_binop` widens to when a register ≥16
// would not fit the compact `/2addr` nibble form. main prints via println(int) only
// (no string-concat invokedynamic), so the class needs no extra desugaring.
public class ArtWideArith {
    static int mix(int n) {
        int a = n + 1, b = n + 2, c = n + 3, d = n + 4, e = n + 5, f = n + 6;
        int g = n + 7, h = n + 8, i = n + 9, j = n + 10, k = n + 11, l = n + 12;
        int m = n + 13, o = n + 14, p = n + 15, q = n + 16, r = n + 17, s = n + 18;
        return a + b + c + d + e + f + g + h + i + j + k + l + m + o + p + q + r + s;
    }

    public static void main(String[] args) {
        System.out.println(mix(0));
        System.out.println(mix(5));
        System.out.println(mix(-3));
    }
}
