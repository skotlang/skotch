public class ArtHandlerPhi {
    static int t(int q) { if (q < 0) throw new RuntimeException("err"); if (q == 99) throw new IllegalStateException("bad"); return q; }

    // sequential int slot versions reaching the handler
    static int seq(int x) {
        int r = 0;
        try { r = 10; t(x); r = 20; t(x - 1); r = 30; } catch (RuntimeException e) { return r; }
        return r;
    }
    // branch-exclusive slot versions in the try
    static int branch(int x) {
        int r = 0;
        try { if (x > 5) { r = 1; t(x); } else { r = 2; t(-x); } r = 9; } catch (RuntimeException e) { return r; }
        return r;
    }
    // used caught variable (move-exception) + multi-version slot
    static int usedCatch(int x) {
        int r = 0;
        try { r = 100; t(x); r = 200; t(x - 1); } catch (RuntimeException e) { return r + e.getMessage().length(); }
        return r;
    }
    // wide (long) slot versions
    static long wide(int x) {
        long r = 0;
        try { r = 1000000000000L; t(x); r = 2000000000000L; t(x - 1); } catch (RuntimeException e) { return r; }
        return r;
    }
    // reference slot versions + two locals with handler-φ
    static String refs(int x) {
        String a = "A"; String b = "B";
        try { a = "X"; b = "Y"; t(x); a = "P"; b = "Q"; t(x - 1); } catch (RuntimeException e) { return a + b; }
        return a + b;
    }
    // multiple catch types
    static int multi(int x) {
        int r = 0;
        try { r = 1; t(x); r = 2; t(x); } catch (IllegalStateException e) { return r * 100; } catch (RuntimeException e) { return r; }
        return r;
    }
    public static void main(String[] z) {
        System.out.println(seq(5) + " " + seq(-1) + " " + seq(0));
        System.out.println(branch(9) + " " + branch(2) + " " + branch(100));
        System.out.println(usedCatch(5) + " " + usedCatch(-1));
        System.out.println(wide(5) + " " + wide(-1));
        System.out.println(refs(5) + " " + refs(-1));
        System.out.println(multi(5) + " " + multi(-1) + " " + multi(99));
    }
}
