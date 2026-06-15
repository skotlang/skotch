import java.util.function.Function;

// A capturing bound method reference whose target takes a PRIMITIVE while the SAM gives a boxed
// value: `s::charAt` captures the String `s`, and Function<Integer,Character>.apply(Object) must
// cast its arg to Integer, UNBOX it to int, call s.charAt(int), then box the char result. The
// synthetic capturing SAM now emits that parameter unbox (invoke-virtual intValue + move-result,
// in place) instead of bailing. Single class (String is built-in), so the ART harness can run it.
public class ArtCapUnbox {
    static Function<Integer, Character> charPicker(String s) {
        return s::charAt;
    }

    public static void main(String[] args) {
        System.out.println(charPicker("hello").apply(1));
        System.out.println(charPicker("world").apply(0));
        System.out.println(charPicker("xyz").apply(2));
    }
}
