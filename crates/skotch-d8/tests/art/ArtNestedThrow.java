public class ArtNestedThrow {
    static int compute(int sel) {
        int r = 0;
        try {
            try {
                if (sel == 1) throw new RuntimeException("a");
                r = 1;
            } catch (RuntimeException e) {
                if (sel == 1) throw new IllegalStateException("b");
                r = 2;
            }
            r = 3;
        } catch (IllegalStateException e2) {
            r = 99;
        }
        return r;
    }
    public static void main(String[] z) {
        System.out.println(compute(0));
        System.out.println(compute(1));
        System.out.println(compute(2));
    }
}
