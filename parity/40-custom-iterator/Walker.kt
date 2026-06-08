// A second user-iterator class to confirm cross-file dispatch.
// `Words.kt` here splits a string by spaces and exposes a
// `WordIterator` returning each piece as a String — proves the
// for-loop's next() descriptor recovery handles reference returns
// (not just primitives like Range2Iter.next(): Int).

class Words(val text: String) {
    operator fun iterator(): WordIterator = WordIterator(text)
}

class WordIterator(val text: String) {
    var pos: Int = 0
    operator fun hasNext(): Boolean = pos < text.length
    operator fun next(): String {
        val sb = StringBuilder()
        while (pos < text.length && text[pos] != ' ') {
            sb.append(text[pos])
            pos = pos + 1
        }
        // Skip trailing space.
        if (pos < text.length) {
            pos = pos + 1
        }
        return sb.toString()
    }
}
