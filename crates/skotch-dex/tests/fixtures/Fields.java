public class Fields {
  static int counter;
  int v;
  static int sumField(Fields f, int n) { int s = 0; for (int i = 0; i < n; i++) s += f.v; return s; }
  static void store(Fields f, int n) { for (int i = 0; i < n; i++) f.v = i; }
  static void bumpStatic(int n) { for (int i = 0; i < n; i++) counter += 1; }
}
