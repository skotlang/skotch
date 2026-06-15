public class ArtNestedTry {
    static int compute(int sel) {
        int r = 0;
        try {
            r = 1;
            try {
                if (sel == 1) throw new IllegalStateException("a");
                r = 2;
            } catch (IllegalStateException e1) {
                r = 100 + e1.getMessage().length();
            }
            if (sel == 2) throw new RuntimeException("bb");
            r = 3;
        } catch (RuntimeException e2) {
            r = 200 + e2.getMessage().length();
        }
        return r;
    }
    public static void main(String[] z) {
        System.out.println(compute(0));
        System.out.println(compute(1));
        System.out.println(compute(2));
    }
}
