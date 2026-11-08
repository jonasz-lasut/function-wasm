// Logging through the host: the world's typed log import (src/world/function,
// wit-bindgen's binding of it), one line with key-value pairs, attached to
// the module's identity in the runtime's log. Never println: it imports
// spectest.print_char, which no host provides.

///|
pub fn debug(msg : String, kv : Array[(String, String)]) -> Unit {
  @function.log(@function.LogLevel::DEBUG, msg, kv)
}

///|
pub fn info(msg : String, kv : Array[(String, String)]) -> Unit {
  @function.log(@function.LogLevel::INFO, msg, kv)
}

///|
pub fn warn(msg : String, kv : Array[(String, String)]) -> Unit {
  @function.log(@function.LogLevel::WARN, msg, kv)
}

///|
pub fn error(msg : String, kv : Array[(String, String)]) -> Unit {
  @function.log(@function.LogLevel::ERROR, msg, kv)
}
