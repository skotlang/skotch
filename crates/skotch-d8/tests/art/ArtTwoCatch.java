public class ArtTwoCatch {
    static int compute(int sel) {
        try {
            if (sel == 1) throw new IllegalStateException("a");
            if (sel == 2) throw new IllegalArgumentException("bb");
            return 0;
        } catch (IllegalStateException e) {
            return 10 + e.getMessage().length();
        } catch (IllegalArgumentException e) {
            return 20 + e.getMessage().length();
        }
    }
    public static void main(String[] z) {
        System.out.println(compute(0));
        System.out.println(compute(1));
        System.out.println(compute(2));
    }
}
