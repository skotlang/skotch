public class ArtMultiThrow {
    static void mayThrow(int sel, int pt) {
        if (sel == pt) throw new RuntimeException();
    }
    static int compute(int sel) {
        int x = 0;
        int result;
        try {
            x = sel + 50;        // defined in try (one version), live into the catch
            mayThrow(sel, 1);    // throw point 1
            mayThrow(sel, 2);    // throw point 2 — same x version at both
            result = 999;
        } catch (RuntimeException e) {
            result = x;          // handler-φ snapshot of x (coalesces: same version at both points)
        }
        return result;
    }
    public static void main(String[] z) {
        System.out.println(compute(0));
        System.out.println(compute(1));
        System.out.println(compute(2));
    }
}
