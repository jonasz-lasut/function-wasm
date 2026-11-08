// The world's root-level `log` import as componentize-js hands it to the
// guest: a default export from a module named after the function.
declare module "log" {
  function log(
    level: "debug" | "info" | "warn" | "error",
    msg: string,
    kv: [string, string][],
  ): void;
  export default log;
}
