//! Official builds supply a repository secret; source builds may omit it.
//! Write into Cargo's private output rather than printing a secret as a Cargo
//! directive (which would also expose it in verbose compiler invocations).
use std::{fs, io::Write, path::Path};

pub const KEY_ENV: &str = "PCL_RH_CURSEFORGE_API_KEY";
pub const FILE_ENV: &str = "PCL_RH_CURSEFORGE_API_KEY_FILE";

pub fn valid(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && value.bytes().all(|b| b.is_ascii_graphic())
}

pub fn generate(out: &Path) -> Result<(), &'static str> {
    println!("cargo:rerun-if-env-changed={KEY_ENV}");
    println!("cargo:rerun-if-env-changed={FILE_ENV}");
    let key = match std::env::var(KEY_ENV) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => match std::env::var(FILE_ENV) {
            Ok(path) => {
                if path.is_empty() || path.chars().any(char::is_control) {
                    return Err("Invalid CurseForge credential file configuration");
                }
                println!("cargo:rerun-if-changed={path}");
                Some(
                    fs::read_to_string(path)
                        .map_err(|_| "Cannot read CurseForge credential file")?
                        .trim_end_matches(['\r', '\n'])
                        .to_owned(),
                )
            }
            Err(std::env::VarError::NotPresent) => None,
            Err(_) => return Err("Invalid CurseForge credential file configuration"),
        },
        Err(_) => return Err("Invalid CurseForge build credential"),
    };
    let source = render(key.as_deref())?;
    let path = out.join("curseforge_credentials.rs");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "Cannot create build credential output")?;
    file.write_all(source.as_bytes())
        .map_err(|_| "Cannot write build credential output")
}

fn render(key: Option<&str>) -> Result<String, &'static str> {
    if key.is_some_and(|value| !valid(value)) {
        return Err("Invalid CurseForge build credential");
    }
    Ok(format!("pub const KEY: &str = {:?};\n", key.unwrap_or("")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_key_is_escaped_and_invalid_input_never_appears_in_errors() {
        assert_eq!(render(None).unwrap(), "pub const KEY: &str = \"\";\n");
        assert_eq!(
            render(Some("a\"\\b")).unwrap(),
            "pub const KEY: &str = \"a\\\"\\\\b\";\n"
        );
        for key in [
            "",
            "private key",
            "private\r\nkey",
            "密钥",
            &"x".repeat(4097),
        ] {
            let error = render(Some(key)).unwrap_err();
            if !key.is_empty() {
                assert!(!error.contains(key));
            }
        }
    }
}
