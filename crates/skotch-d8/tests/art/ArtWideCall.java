// >16-register method making a multi-arg static call whose arguments are the method's own
// PARAMETERS. Parameters are allocated to LOW registers but the args-high remap moves them to
// HIGH final registers — so the compact 35c invoke form would pass the allocated-register check
// yet overflow its 4-bit arg nibble after remap. The fix detects the high FINAL register and
// emits invoke/range instead of bailing. main prints via println(int) only (no string-concat
// invokedynamic).
public class ArtWideCall {
    static int sink(int a, int b, int c, int d) {
        return a * 1000 + b * 100 + c * 10 + d;
    }

    static int run(int w, int x, int y, int z) {
        int a = w + 1, b = w + 2, c = w + 3, d = w + 4, e = w + 5, f = w + 6;
        int g = w + 7, h = w + 8, i = w + 9, j = w + 10, k = w + 11, l = w + 12;
        int m = w + 13, o = w + 14, p = w + 15, q = w + 16, r = w + 17;
        // The four arguments are the parameters: low allocated registers, high final registers.
        int t = sink(w, x, y, z);
        return a + b + c + d + e + f + g + h + i + j + k + l + m + o + p + q + r + t;
    }

    public static void main(String[] args) {
        System.out.println(run(0, 1, 2, 3));
        System.out.println(run(5, 6, 7, 8));
        System.out.println(run(-1, -2, -3, -4));
    }
}
