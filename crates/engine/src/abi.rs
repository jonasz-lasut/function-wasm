//! The ABI v1 shape check: verifies the exports and imports before a module
//! is cached, so a wrong module fails once at load rather than on every run.
//! The refusal strings match the Go runtime's `internal/engine.checkABI`
//! verbatim: they are the contract `guestfn build`/`push` and operators see.

use std::collections::HashMap;

use wasmtime::{ExternType, FuncType, ValType};

use crate::{
    EXPORT_ALLOC, EXPORT_INITIALIZE, EXPORT_MEMORY, EXPORT_RUN, Error, HOST_HTTP, HOST_LOG,
    HOST_MODULE, WASI_MODULE,
};

pub(crate) fn check_abi(m: &wasmtime::Module) -> Result<(), Error> {
    let exports: HashMap<&str, ExternType> = m.exports().map(|e| (e.name(), e.ty())).collect();

    match exports.get(EXPORT_MEMORY) {
        Some(ExternType::Memory(_)) => {}
        _ => {
            return Err(Error(format!(
                "module does not export a memory named {EXPORT_MEMORY:?}"
            )));
        }
    }
    check_func(&exports, EXPORT_ALLOC, &[ValType::I32], &[ValType::I32])?;
    check_func(
        &exports,
        EXPORT_RUN,
        &[ValType::I32, ValType::I32],
        &[ValType::I64],
    )?;
    if exports.contains_key(EXPORT_INITIALIZE) {
        check_func(&exports, EXPORT_INITIALIZE, &[], &[])?;
    }

    for im in m.imports() {
        let (module, name) = (im.module(), im.name());
        match (module, name) {
            (WASI_MODULE, _) => {}
            (HOST_MODULE, HOST_LOG) => {
                check_import(
                    &im.ty(),
                    HOST_LOG,
                    &[ValType::I32, ValType::I32, ValType::I32],
                    &[],
                )?;
            }
            (HOST_MODULE, HOST_HTTP) => {
                check_import(
                    &im.ty(),
                    HOST_HTTP,
                    &[ValType::I32, ValType::I32],
                    &[ValType::I64],
                )?;
            }
            _ => {
                return Err(Error(format!(
                    "module imports {module}.{name}, which the host does not provide"
                )));
            }
        }
    }
    Ok(())
}

fn check_func(
    exports: &HashMap<&str, ExternType>,
    name: &str,
    params: &[ValType],
    results: &[ValType],
) -> Result<(), Error> {
    let Some(ty) = exports.get(name) else {
        return Err(Error(format!("module does not export {name:?}")));
    };
    let ExternType::Func(ft) = ty else {
        return Err(Error(format!("export {name:?} is not a function")));
    };
    if !type_matches(ft, params, results) {
        return Err(Error(format!(
            "export {name:?} has signature {}, ABI v1 requires {}",
            signature_of(ft),
            signature(params, results)
        )));
    }
    Ok(())
}

fn check_import(
    ty: &ExternType,
    name: &str,
    params: &[ValType],
    results: &[ValType],
) -> Result<(), Error> {
    let matches = match ty {
        ExternType::Func(ft) => type_matches(ft, params, results),
        _ => false,
    };
    if !matches {
        return Err(Error(format!(
            "module imports {HOST_MODULE}.{name} with the wrong type, ABI v1 requires {}",
            signature(params, results)
        )));
    }
    Ok(())
}

fn type_matches(ft: &FuncType, params: &[ValType], results: &[ValType]) -> bool {
    ft.params().len() == params.len()
        && ft.results().len() == results.len()
        && ft
            .params()
            .zip(params)
            .all(|(got, want)| kind(&got) == kind(want))
        && ft
            .results()
            .zip(results)
            .all(|(got, want)| kind(&got) == kind(want))
}

pub(crate) fn signature_of(ft: &FuncType) -> String {
    signature(
        &ft.params().collect::<Vec<_>>(),
        &ft.results().collect::<Vec<_>>(),
    )
}

fn signature(params: &[ValType], results: &[ValType]) -> String {
    format!("({}) -> ({})", kinds(params), kinds(results))
}

fn kinds(types: &[ValType]) -> String {
    types.iter().map(kind).collect::<Vec<_>>().join(", ")
}

fn kind(ty: &ValType) -> &'static str {
    match ty {
        ValType::I32 => "i32",
        ValType::I64 => "i64",
        ValType::F32 => "f32",
        ValType::F64 => "f64",
        ValType::V128 => "v128",
        ValType::Ref(_) => "ref",
    }
}
