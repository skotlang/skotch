// >16-register method WITH a loop/branch — exercises the SSA φ-pipeline and branch
// fixups at high register counts (offset/spill safety). Sixteen accumulators plus the
// induction var, the bound, and temporaries push register pressure past 16; the loop
// body's adds widen to 3-address forms where a register ≥16 appears. main prints via
// println(int) only (no string-concat invokedynamic).
public class ArtWideLoop {
    static int run(int n) {
        int s0 = 0, s1 = 0, s2 = 0, s3 = 0, s4 = 0, s5 = 0, s6 = 0, s7 = 0;
        int s8 = 0, s9 = 0, s10 = 0, s11 = 0, s12 = 0, s13 = 0, s14 = 0, s15 = 0;
        for (int t = 0; t < n; t++) {
            s0 += t; s1 += t + 1; s2 += t + 2; s3 += t + 3;
            s4 += t + 4; s5 += t + 5; s6 += t + 6; s7 += t + 7;
            s8 += t + 8; s9 += t + 9; s10 += t + 10; s11 += t + 11;
            s12 += t + 12; s13 += t + 13; s14 += t + 14; s15 += t + 15;
        }
        return s0 + s1 + s2 + s3 + s4 + s5 + s6 + s7
             + s8 + s9 + s10 + s11 + s12 + s13 + s14 + s15;
    }

    public static void main(String[] args) {
        System.out.println(run(10));
        System.out.println(run(0));
        System.out.println(run(3));
    }
}
