import java.util.*;
import java.util.function.Supplier;
public class ArtCovRet {
    static ArrayList<String> mkList() { ArrayList<String> l = new ArrayList<>(); l.add("a"); l.add("b"); l.add("c"); return l; }
    static HashMap<String,Integer> mkMap() { HashMap<String,Integer> m = new HashMap<>(); m.put("x", 7); m.put("y", 9); return m; }
    static int sz(Supplier<? extends Collection<String>> s) { return s.get().size(); }
    static int msz(Supplier<? extends Map<String,Integer>> s) { return s.get().size(); }
    public static void main(String[] a) {
        System.out.println(sz(ArtCovRet::mkList));   // ArrayList -> Collection (covariant)
        System.out.println(msz(ArtCovRet::mkMap));   // HashMap -> Map (covariant)
    }
}
