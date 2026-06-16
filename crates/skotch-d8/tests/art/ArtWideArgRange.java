public class ArtWideArgRange {
    static long g6(int a, int b, int c, int d, int e, int f) { return a + b + c + d + e + f; } // 6 words → range
    static long use(long w) { return (w & 0xffL) + 1; }                                        // 35c wide arg

    // num_arg=6 (4 ints + long w at arg regs (4,5)). No int↔long conversions in `run`. At
    // registers_size=17, w's low half remaps to v15 (old check misses), high half to v16 (the fix).
    static long run(int a, int b, int c, int d, long w) {
        long acc = w;
        for (int i = 0; i < 5; i++) {
            acc = acc + g6(a, b, c, d, a, i);   // 6-word range invoke (scratch)
            acc = acc + use(w);                 // wide arg w → 35c invoke
        }
        return acc;
    }
    public static void main(String[] args) {
        System.out.println(run(1, 2, 3, 4, 100L));
        System.out.println(run(6, 7, 8, 9, 1000L));
    }
}
