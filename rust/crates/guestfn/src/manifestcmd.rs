//! guestfn manifest: the wasmfn.yaml guestfn push publishes beside the
//! module, and the copy an artifact carries.

use std::path::PathBuf;

use function_wasm::location::parse_any_reference;
use function_wasm::manifest::Manifest;
use function_wasm::oci;

#[derive(clap::Subcommand, Debug)]
pub enum ManifestCmd {
    /// Check a manifest file (wasmfn.yaml) the way guestfn build and push
    /// do, and print what it declares.
    Validate(ValidateCmd),
    /// Print the manifest an artifact carries as its manifest layer,
    /// without pulling the module.
    Show(ShowCmd),
}

impl ManifestCmd {
    pub fn run(&self) -> Result<(), String> {
        match self {
            ManifestCmd::Validate(cmd) => cmd.run(),
            ManifestCmd::Show(cmd) => cmd.run(),
        }
    }
}

#[derive(clap::Args, Debug)]
pub struct ValidateCmd {
    /// The manifest file (default wasmfn.yaml).
    #[arg(default_value = function_wasm::manifest::FILE_NAME)]
    file: PathBuf,
}

impl ValidateCmd {
    fn run(&self) -> Result<(), String> {
        let m = Manifest::load(&self.file)?;
        println!(
            "{}: valid ({})",
            self.file.display(),
            crate::inspect::manifest_text(&m)
        );
        Ok(())
    }
}

#[derive(clap::Args, Debug)]
pub struct ShowCmd {
    /// An OCI reference (tag or digest) of an artifact guestfn push
    /// published.
    r#ref: String,

    /// yaml or json.
    #[arg(long, default_value = "yaml", value_parser = ["yaml", "json"])]
    output: String,
}

impl ShowCmd {
    fn run(&self) -> Result<(), String> {
        let reference =
            parse_any_reference(&self.r#ref).map_err(|e| format!("cannot parse reference: {e}"))?;
        let client = crate::inspect::client_for(&reference);
        let (raw, digest) = client.raw_manifest(&reference.manifest_ref())?;
        let om: oci::OciManifest = serde_json::from_slice(&raw)
            .map_err(|e| format!("cannot parse manifest {digest}: {e}"))?;
        let Some(ml) = oci::manifest_layer(&om) else {
            return Err(format!(
                "{} carries no {} layer: it was pushed without a {} (guestfn push publishes the manifest beside the module)",
                self.r#ref,
                oci::MANIFEST_LAYER_TYPE,
                function_wasm::manifest::FILE_NAME
            ));
        };
        let m = crate::inspect::fetch_manifest(&client, &ml)
            .map_err(|e| format!("{}: {e}", self.r#ref))?;
        if self.output == "json" {
            println!("{}", String::from_utf8_lossy(&m.json()?));
            return Ok(());
        }
        print!("{}", serde_yaml::to_string(&m).map_err(|e| e.to_string())?);
        Ok(())
    }
}
