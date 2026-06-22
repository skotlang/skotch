public class ArtCastArray {
    static int boolElem(Object o, int i) { boolean[] a = (boolean[]) o; return a[i] ? 1 : 0; }
    static int byteElem(Object o, int i) { byte[] a = (byte[]) o; return a[i]; }
    static void boolSet(Object o, int i, boolean v) { boolean[] a = (boolean[]) o; a[i] = v; }
    public static void main(String[] z) {
        boolean[] b = { false, true, false };
        byte[] by = { 10, 20, 30 };
        System.out.println(boolElem(b, 1));
        System.out.println(byteElem(by, 2));
        boolSet(b, 0, true);
        System.out.println(boolElem(b, 0));
    }
}
