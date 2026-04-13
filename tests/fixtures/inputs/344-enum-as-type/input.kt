enum class Dir { NORTH, SOUTH, EAST, WEST }

fun move(d: Dir): String = when (d) {
    Dir.NORTH -> "up"
    Dir.SOUTH -> "down"
    Dir.EAST -> "right"
    Dir.WEST -> "left"
    else -> "?"
}

fun main() {
    println(move(Dir.NORTH))
    println(move(Dir.WEST))
}
