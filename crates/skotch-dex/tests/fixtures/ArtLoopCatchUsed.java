public class ArtLoopCatchUsed {
    static int compute(int n) {
        int s = 0;
        for (int i = 0; i < n; i++) {
            try { if (i == 1) throw new RuntimeException("xy"); s += i; }
            catch (RuntimeException e) { s += e.getMessage().length(); }  // used e, INSIDE loop
        }
        return s;
    }
    public static void main(String[] z) { System.out.println(compute(4)); }
}
