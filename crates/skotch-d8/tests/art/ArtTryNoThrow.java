public class ArtTryNoThrow {
    // try/finally over a NON-throwing body: javac emits a catch-all over the body, but the
    // exceptional finally-copy handler is unreachable (the body can't throw). skotch drops the
    // dead region + handler block; the finally still runs inline on the normal path.
    static int compute(int x) {
        int r;
        try { r = x + 1; } finally { System.out.println("fin"); }
        return r;
    }
    public static void main(String[] z) {
        System.out.println(compute(10));
        System.out.println(compute(-3));
    }
}
