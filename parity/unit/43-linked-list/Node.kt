// Generic Node with a self-referential nullable next pointer.
// `Node<T>?` as a property type — the field can be null OR a same-
// shaped Node<T>. Tests:
//   - generic class with one type param + self-reference
//   - nullable property holding a reference to the same generic type
//   - var property mutation (next can be reassigned)

class Node<T>(val value: T, var next: Node<T>?)
