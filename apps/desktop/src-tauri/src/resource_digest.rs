//! Official digest requirements stay explicit at the provider boundary. The
//! transfer also computes SHA512 locally, but that is not a substitute for a
//! provider's expected SHA1 or SHA512 when establishing download integrity.
use sha2::{Digest, Sha512};

pub enum ExpectedDigest<'a> {
    Sha512(&'a str),
    Sha1(&'a str),
}

impl ExpectedDigest<'_> {
    pub fn validate(&self) -> Result<(), String> {
        let (value, length) = match self {
            Self::Sha512(value) => (*value, 128),
            Self::Sha1(value) => (*value, 40),
        };
        if value.len() == length && value.bytes().all(|ch| ch.is_ascii_hexdigit()) {
            Ok(())
        } else {
            Err("官方文件校验值无效".into())
        }
    }

    pub fn hasher(&self) -> FileHasher {
        FileHasher {
            sha512: Sha512::new(),
            sha1: matches!(self, Self::Sha1(_)).then(sha1::Sha1::new),
        }
    }
}

pub struct FileHasher {
    sha512: Sha512,
    sha1: Option<sha1::Sha1>,
}

impl FileHasher {
    pub fn update(&mut self, bytes: &[u8]) {
        self.sha512.update(bytes);
        if let Some(hash) = &mut self.sha1 {
            hash.update(bytes);
        }
    }

    pub fn verify(self, expected: &ExpectedDigest<'_>) -> Result<String, String> {
        expected.validate()?;
        let sha512 = format!("{:x}", self.sha512.finalize());
        let actual = match expected {
            ExpectedDigest::Sha512(_) => sha512.clone(),
            ExpectedDigest::Sha1(_) => {
                format!("{:x}", self.sha1.ok_or("文件校验算法不匹配")?.finalize())
            }
        };
        let value = match expected {
            ExpectedDigest::Sha512(value) | ExpectedDigest::Sha1(value) => value,
        };
        if actual.eq_ignore_ascii_case(value) {
            Ok(sha512)
        } else {
            let algorithm = match expected {
                ExpectedDigest::Sha512(_) => "SHA512",
                ExpectedDigest::Sha1(_) => "SHA1",
            };
            Err(format!("文件{algorithm}校验失败，暂存内容已丢弃"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_sha1_is_checked_before_returning_local_sha512() {
        let digest = ExpectedDigest::Sha1("a9993e364706816aba3e25717850c26c9cd0d89d");
        let mut hash = digest.hasher();
        hash.update(b"abc");
        assert_eq!(
            hash.verify(&digest).unwrap(),
            format!("{:x}", Sha512::digest(b"abc"))
        );
        let mut bad = digest.hasher();
        bad.update(b"abd");
        assert!(bad.verify(&digest).is_err());
        assert!(
            ExpectedDigest::Sha512("a9993e364706816aba3e25717850c26c9cd0d89d")
                .validate()
                .is_err()
        );
    }
}
