// `synchronized(lock){...}` — monitor-enter/exit + the implicit self-covering catch-all
// handler (dropped as functionally-exact). The output proves the body ran under the lock and
// returned correctly; the ART harness also proves no deadlock (it must not hang).
public class ArtSync {
    static final Object lock = new Object();
    static int counter = 0;
    static int bump(int n) {
        synchronized (lock) {
            counter += n;
            return counter;
        }
    }
    static int total(int[] a) {
        int s = 0;
        for (int x : a) {
            synchronized (lock) { s += x; }
        }
        return s;
    }
    public static void main(String[] args) {
        System.out.println(bump(5));
        System.out.println(bump(3));
        System.out.println(bump(10));
        System.out.println(total(new int[] {1, 2, 3, 4}));
    }
}
