// TODO: sealed class hierarchy.
sealed class Result
class Ok(val value: Int) : Result()
class Err(val message: String) : Result()
