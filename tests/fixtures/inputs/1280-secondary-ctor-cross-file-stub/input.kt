// Regression for audit fix #2: ExternalClassDecl.secondary_ctors is
// populated and flows into MirClass.secondary_constructors on
// cross-file stubs, so the backend's constructor-arity dispatcher
// picks the right `<init>` descriptor for both the primary and each
// secondary constructor.
//
// Single-file fixture covers the in-file path; the cross-file
// equivalent is exercised by the multi-file `skotch-examples`
// projects. The data flow is the same — gather → stub-builder →
// backend's `secondary_constructors` iteration.
class Point(val x: Int, val y: Int) {
    // Secondary constructor — delegates to primary with zeros.
    constructor() : this(0, 0)
    // Another secondary — delegates with a single coord (square).
    constructor(side: Int) : this(side, side)
}

fun main() {
    val origin = Point()
    println("${origin.x},${origin.y}")
    val pair = Point(3, 4)
    println("${pair.x},${pair.y}")
    val square = Point(7)
    println("${square.x},${square.y}")
}
