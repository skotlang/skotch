public class ArtTwoConst {
    // Forces the SSA path (loop) where two small consts are EACH rematerialized (all uses
    // lit-foldable) yet collide on one binop `a + b` — both operands would be NO_REG, which
    // emit_binop can't fold (a lit form has one register + one literal). The fix keeps the LEFT
    // const in a register so the RIGHT folds via reg(left).
    static int g(int n) {
        int a = 7;
        int b = -1;
        int s = a + b;                 // two-rematerializable-const binop
        for (int i = 0; i < n; i++) {
            s += a * i;                // a folds as mul-int/lit8 (or 3-addr once a holds a reg)
            s += b * i;                // b folds as mul-int/lit8 (still rematerialized)
        }
        return s;
    }
    public static void main(String[] args) {
        System.out.println(g(0));      // 6
        System.out.println(g(1));      // 6
        System.out.println(g(5));      // 66
        System.out.println(g(10));     // 276
    }
}
