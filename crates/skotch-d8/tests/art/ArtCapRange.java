public class ArtCapRange {
    interface Sink { String apply(String a, String b); }

    // 6 reference args (4 captured + 2 SAM params) — the impl the capturing lambda forwards to.
    static String join(String c1, String c2, String c3, String c4, String a, String b) {
        StringBuilder sb = new StringBuilder();
        sb.append(c1); sb.append(c2); sb.append(c3); sb.append(c4); sb.append(a); sb.append(b);
        return sb.toString();
    }

    static Sink make(String c1, String c2, String c3, String c4) {
        return (a, b) -> join(c1, c2, c3, c4, a, b); // captures c1..c4, params a,b → 6 arg-words
    }

    public static void main(String[] args) {
        Sink s = make("A", "B", "C", "D");
        System.out.println(s.apply("x", "y"));
        System.out.println(make("1", "2", "3", "4").apply("5", "6"));
    }
}
