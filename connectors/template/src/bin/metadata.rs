use std::env;
use std::error::Error;
use std::fs;

fn main() -> Result<(), Box<dyn Error>> {
    let output = env::args()
        .nth(1)
        .ok_or("usage: metadata OUTPUT_DIRECTORY")?;
    let encoded = mav_connector_template::metadata()?.encode()?;
    fs::create_dir_all(&output)?;
    fs::write(format!("{output}/manifest.cbor"), encoded.manifest)?;
    fs::write(format!("{output}/abi.cbor"), encoded.abi)?;
    fs::write(format!("{output}/fixtures.cbor"), encoded.fixtures)?;
    Ok(())
}
