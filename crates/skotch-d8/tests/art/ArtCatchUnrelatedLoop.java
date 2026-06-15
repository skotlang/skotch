public class ArtCatchUnrelatedLoop {
    static int compute(int n, boolean boom) {
        int caught = 0;
        try {
            if (boom) throw new RuntimeException("boom");
            caught = 1;
        } catch (RuntimeException e) {
            caught = e.getMessage().length();   // USES the caught variable e
        }
        int sum = 0;
        for (int i = 0; i < n; i++) sum += i;   // UNRELATED loop, after the try/catch
        return caught * 1000 + sum;
    }
    public static void main(String[] z) {
        System.out.println(compute(3, false));
        System.out.println(compute(3, true));
        System.out.println(compute(0, true));
    }
}
