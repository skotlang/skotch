public class ArtHiLocalIf {
    // f forces ~18 simultaneously-live int locals so one lands at an allocated register >=16,
    // and that high local is an operand of a two-register if-test (22t, no wider DEX form) — the
    // case the dexer must spill through low scratch before the compare.
    static int f(int p) {
        int a0 = p + 1;  int a1 = p + 2;  int a2 = p + 3;  int a3 = p + 4;
        int a4 = p + 5;  int a5 = p + 6;  int a6 = p + 7;  int a7 = p + 8;
        int a8 = p + 9;  int a9 = p + 10; int a10 = p + 11; int a11 = p + 12;
        int a12 = p + 13; int a13 = p + 14; int a14 = p + 15; int a15 = p + 16;
        int hi = p * 2;             // data-dependent, want this allocated >=16
        int thresh = 30;
        int s = a0+a1+a2+a3+a4+a5+a6+a7+a8+a9+a10+a11+a12+a13+a14+a15+hi+thresh;
        if (hi > thresh) {          // two-reg if-test on a high local; both branches reachable
            s = s + 1000;
        }
        return s;
    }
    public static void main(String[] args) {
        System.out.println(f(5));    // hi=10  -> 10<=30, false branch
        System.out.println(f(20));   // hi=40  -> 40>30,  true branch
        System.out.println(f(100));  // hi=200 -> true
    }
}
