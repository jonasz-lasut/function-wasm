// The world's run export: the one hand-written file among the generated
// bindings, the slot top.mbt declares (wit-bindgen's MoonBit generator
// declares the export here and leaves its body to the guest). The glue in
// src/wasmfn does the work.

///|
pub fn run(request : FixedArray[Byte]) -> Result[FixedArray[Byte], String] {
  @wasmfn.handle(request)
}
