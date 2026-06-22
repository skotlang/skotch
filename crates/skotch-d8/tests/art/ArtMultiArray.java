public class ArtMultiArray {
    static int sumInt(int a, int b) {
        int[][] m = new int[a][b];           // primitive 2D, variable dims
        int n = 0;
        for (int i = 0; i < a; i++)
            for (int j = 0; j < b; j++) { m[i][j] = i * 10 + j; n += m[i][j]; }
        return n + m.length + m[0].length;
    }
    static String joinStr(int a, int b) {
        String[][] s = new String[a][b];     // reference 2D, variable dims
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < a; i++)
            for (int j = 0; j < b; j++) { s[i][j] = i + ":" + j; sb.append(s[i][j]); sb.append(','); }
        return sb.toString() + s.length + "/" + s[0].length;
    }
    public static void main(String[] x) {
        System.out.println(sumInt(3, 4));
        System.out.println(joinStr(2, 3));
        char[][] c = new char[0][0];          // const-0 2D (ArrayBasedEscaperMap shape)
        System.out.println(c.length);
        double[][] d = new double[2][2];      // wide primitive base
        d[1][1] = 3.5; System.out.println(d[1][1] + " " + d.length);
    }
}
