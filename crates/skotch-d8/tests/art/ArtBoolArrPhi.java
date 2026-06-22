public class ArtBoolArrPhi {
    // A loop-carried boolean[] reassigned in the loop forms a φ-CYCLE: array_desc resolves the
    // byte-vs-boolean variant by skipping the cyclic back-edge operand and using a resolvable one.
    static boolean[] f(boolean[] a, boolean[] b, int n) {
        boolean[] arr = a;
        for (int i = 0; i < n; i++) {
            if ((i & 1) == 0) arr = b;
            arr[i % arr.length] ^= true;
        }
        return arr;
    }
    static String show(boolean[] r) { StringBuilder s = new StringBuilder(); for (boolean v : r) s.append(v ? '1' : '0'); return s.toString(); }
    public static void main(String[] x) {
        System.out.println(show(f(new boolean[]{true,false,true}, new boolean[]{false,false,false}, 5)));
        System.out.println(show(f(new boolean[]{false,false}, new boolean[]{true,true}, 4)));
    }
}
