// Exercises dup_x2 (0x5b) and dup2_x1 (0x5d), which the SSA stack simulator didn't model.
//  - `ia[i] += v` returning the result compiles to dup_x2 (form 1: dup the cat-1 result past
//    the array ref + index).
//  - `this.longField += v` / `this.doubleField += v` returning the result compiles to dup2_x1
//    (form 2: dup the category-2/wide result past the object ref).
// These are pure operand-stack reorders (no new instruction), so the output simply depends on
// the reordered values being correct; ART execution proves the shuffle is right.
public class ArtDupX {
    static int[] ia = new int[4];
    long lf = 0;
    double df = 0;

    static int arrBump(int i, int v) {
        return ia[i] += v;
    }

    long longBump(long v) {
        return this.lf += v;
    }

    double dblBump(double v) {
        return this.df += v;
    }

    public static void main(String[] args) {
        System.out.println(arrBump(0, 5));
        System.out.println(arrBump(0, 3));
        System.out.println(arrBump(2, 7));
        ArtDupX o = new ArtDupX();
        System.out.println(o.longBump(100L));
        System.out.println(o.longBump(50L));
        System.out.println(o.dblBump(2.5));
        System.out.println(o.dblBump(1.5));
    }
}
