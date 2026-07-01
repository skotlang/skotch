// Sealed `Shape` hierarchy with three concrete subclasses and two
// abstract methods (area + perimeter). Each subclass overrides both.
// Demonstrates polymorphic method dispatch from a sealed-class root
// without going through any generic container (which would erase
// the element type and trip the generic-arg propagation gap).
//
// Sophistication step over example 33:
//   - sealed class with multiple `abstract fun` declarations
//   - 3 concrete subclasses, each overriding both abstracts
//   - direct method-call dispatch (`shape.area()`, `shape.perimeter()`)
//     against polymorphic receivers; no `when (is X)` pattern matching
//   - mixed Int/Double arithmetic in subclass methods

sealed class Shape {
    abstract fun area(): Double
    abstract fun perimeter(): Double
    abstract fun describe(): String
}

class Circle(val radius: Double) : Shape() {
    override fun area(): Double {
        return 3.141592653589793 * radius * radius
    }
    override fun perimeter(): Double {
        return 2.0 * 3.141592653589793 * radius
    }
    override fun describe(): String {
        return "circle(r=$radius)"
    }
}

class Rectangle(val w: Double, val h: Double) : Shape() {
    override fun area(): Double {
        return w * h
    }
    override fun perimeter(): Double {
        return 2.0 * (w + h)
    }
    override fun describe(): String {
        return "rectangle(${w}x$h)"
    }
}

class Triangle(val base: Double, val height: Double) : Shape() {
    override fun area(): Double {
        return 0.5 * base * height
    }
    override fun perimeter(): Double {
        // assume isoceles for simplicity: 2 equal sides of length
        // sqrt((base/2)^2 + height^2), plus base
        val halfBase = base / 2.0
        val sideSq = halfBase * halfBase + height * height
        val side = sqrt(sideSq)
        return base + 2.0 * side
    }
    override fun describe(): String {
        return "triangle(base=$base, h=$height)"
    }
}

fun absD(x: Double): Double {
    if (x < 0.0) return -x
    return x
}

fun sqrt(x: Double): Double {
    if (x == 0.0) return 0.0
    var guess = x / 2.0
    var i = 0
    while (i < 50) {
        val next = (guess + x / guess) / 2.0
        if (absD(next - guess) < 1.0e-10) {
            return next
        }
        guess = next
        i = i + 1
    }
    return guess
}

fun report(s: Shape) {
    println(s.describe() + " area=" + s.area() + " perimeter=" + s.perimeter())
}

fun main() {
    val c = Circle(5.0)
    val r = Rectangle(4.0, 6.0)
    val t = Triangle(3.0, 8.0)

    report(c)
    report(r)
    report(t)

    val totalArea = c.area() + r.area() + t.area()
    val totalPerimeter = c.perimeter() + r.perimeter() + t.perimeter()
    println("total area = $totalArea")
    println("total perimeter = $totalPerimeter")
}
